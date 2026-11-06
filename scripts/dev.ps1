<#
.SYNOPSIS
  一键启动本地开发环境：后端（streamable-http）+ 前端（Vite dev）。
.DESCRIPTION
  用法：pwsh scripts/dev.ps1 [-BackendPort 8000] [-WebPort 5173] [-PgDsn <dsn>] [-ResetData] [-NoWeb]
    -BackendPort  后端 HTTP 端口（默认 8000）
    -WebPort      前端 Vite 端口（默认 5173，被占用时自动 +1 顺延）
    -PgDsn        可选。给了就生成 .dev-data/policy.toml（追加 dev-pg 数据源）并注入 HJZL_DEV_PG_DSN
    -ResetData    启动前清空 .dev-data/（默认保留，便于连续试用）
    -NoWeb        只起后端
  Ctrl+C（或关窗）会同时停掉前后端子进程；日志在 .dev-data/logs/。
#>
[CmdletBinding()]
param(
  [int]$BackendPort = 8000,
  [int]$WebPort = 5173,
  [string]$PgDsn = '',
  [switch]$ResetData,
  [switch]$NoWeb
)

$ErrorActionPreference = 'Continue'
Set-StrictMode -Version 2.0

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Split-Path -Parent $ScriptDir
$BackendDir = Join-Path $RepoRoot 'backend'
$WebDir = Join-Path $RepoRoot 'admin-web'
$DataDir = Join-Path $RepoRoot '.dev-data'
$LogDir = Join-Path $DataDir 'logs'
$BasePolicy = Join-Path $RepoRoot 'config/policy.toml'

$AdminTokens = 'alice:tok_alice_000000000,bob:tok_bob_00000000000'
$AliceToken = 'tok_alice_000000000'
$BobToken = 'tok_bob_00000000000'

$script:Children = New-Object System.Collections.ArrayList
$script:Stopping = $false

function Resolve-Tool {
  param([string]$Name, [string[]]$Candidates)
  foreach ($c in $Candidates) {
    $found = @(Get-Command -Name $c -CommandType Application -ErrorAction SilentlyContinue)
    if ($found.Count -gt 0) { return $found[0].Source }
  }
  return $null
}

function Test-PortFree {
  # 端口未被占用返回 $true（node 试绑一下）
  param([int]$Port)
  & node -e 'const net=require("net");const s=net.createServer();s.once("error",()=>process.exit(1));s.listen(Number(process.argv[1]),"127.0.0.1",()=>s.close(()=>process.exit(0)))' $Port | Out-Null
  return ($LASTEXITCODE -eq 0)
}

function Wait-Http {
  # 等待 URL 在 $Seconds 秒内返回 200；成功返回响应体，失败返回 $null
  param([string]$Url, [int]$Seconds, [string]$Curl)
  $deadline = (Get-Date).AddSeconds($Seconds)
  while ((Get-Date) -lt $deadline) {
    $body = Join-Path $env:TEMP ("hjzl-dev-" + [System.Guid]::NewGuid().ToString('N').Substring(0, 6) + '.txt')
    $code = (& $Curl -sS --connect-timeout 2 --max-time 3 -o $body -w '%{http_code}' $Url 2>$null | Out-String).Trim()
    if ($code -eq '200') {
      $text = ''
      if (Test-Path $body) { $text = (Get-Content $body -Raw -ErrorAction SilentlyContinue) }
      Remove-Item $body -Force -ErrorAction SilentlyContinue
      return $text
    }
    Remove-Item $body -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
  }
  return $null
}

function Stop-Children {
  if ($script:Stopping) { return }
  $script:Stopping = $true
  foreach ($c in $script:Children) {
    Write-Host ("==> 停止 {0}（pid {1}）" -f $c.Name, $c.Id)
    if ($env:OS -eq 'Windows_NT') {
      & taskkill /PID $c.Id /T /F 2>&1 | Out-Null
    } else {
      & pkill -TERM -P $c.Id 2>&1 | Out-Null
      & kill -TERM $c.Id 2>&1 | Out-Null
    }
  }
  Start-Sleep -Milliseconds 800
  $left = @()
  foreach ($c in $script:Children) {
    if (Get-Process -Id $c.Id -ErrorAction SilentlyContinue) { $left += $c.Name }
  }
  if ($left.Count -gt 0) {
    Write-Host ("警告：以下进程仍在运行：{0}" -f ($left -join ', ')) -ForegroundColor Yellow
  } else {
    Write-Host '前后端进程已全部退出（无残留）'
  }
}

# Ctrl+C：拦下默认行为，先收掉子进程，再由主流程退出
$script:CancelHandler = [System.ConsoleCancelEventHandler] {
  param($e, $s)
  $e.Cancel = $true
  Stop-Children
}
[Console]::add_CancelKeyPress($script:CancelHandler)

try {
  # ------------------------------------------------------------ 准备数据目录
  if ($ResetData -and (Test-Path $DataDir)) {
    Write-Host "==> 清空 $DataDir"
    # .dev-data/node_modules 可能是指向 admin-web/node_modules 的链接，先摘链接再删目录
    $link = Join-Path $DataDir 'node_modules'
    if (Test-Path $link) { & cmd.exe /c rmdir $link 2>&1 | Out-Null }
    Remove-Item -Recurse -Force $DataDir -ErrorAction SilentlyContinue
  }
  New-Item -ItemType Directory -Force -Path $LogDir | Out-Null

  $cargo = Resolve-Tool 'cargo' @('cargo', 'cargo.exe')
  $node = Resolve-Tool 'node' @('node', 'node.exe')
  $npm = Resolve-Tool 'npm' @('npm.cmd', 'npm.exe', 'npm')
  $curl = Resolve-Tool 'curl' @('curl.exe', 'curl')
  $missing = @()
  foreach ($pair in @(@('cargo', $cargo), @('node', $node), @('npm', $npm), @('curl', $curl))) {
    if ($null -eq $pair[1]) { $missing += $pair[0] }
  }
  if ($missing.Count -gt 0) {
    Write-Host ("缺少必需工具：{0}" -f ($missing -join ' ')) -ForegroundColor Red
    Write-Host '安装指引：cargo → https://rustup.rs；node/npm → https://nodejs.org；curl → apt/brew install curl'
    exit 1
  }

  # ------------------------------------------------------------ 配置：可选 PG 数据源
  $policyFile = $BasePolicy
  $backendEnv = @{}
  if ($PgDsn -ne '') {
    $policyFile = Join-Path $DataDir 'policy.toml'
    Copy-Item $BasePolicy $policyFile -Force
    $dsnBlock = @"

[[datasources]]
name = "dev-pg"
engine = "postgres"
readonly = true
allowed_schemas = ["public"]
statement_timeout_ms = 15000
dsn_env = "HJZL_DEV_PG_DSN"
"@
    Add-Content -Path $policyFile -Value $dsnBlock -Encoding utf8
    $backendEnv['HJZL_DEV_PG_DSN'] = $PgDsn
    Write-Host "==> 已生成临时数据源配置：$policyFile（数据源 dev-pg，DSN 走 HJZL_DEV_PG_DSN）"
  } else {
    Write-Host '==> 未提供 -PgDsn：使用 config/policy.toml 原样（db_query_readonly 会返回 MCP_NOT_IMPLEMENTED，属预期）'
  }

  # ------------------------------------------------------------ 后端
  $exeName = 'hjzl-server'
  if ($env:OS -eq 'Windows_NT') { $exeName = 'hjzl-server.exe' }
  $serverBin = Join-Path $BackendDir ("target/debug/$exeName")
  if (-not (Test-Path $serverBin)) {
    Write-Host '==> 首次运行：cargo build --bin hjzl-server'
    Push-Location $BackendDir
    try { & cargo build --bin hjzl-server } finally { Pop-Location }
    if (-not (Test-Path $serverBin)) {
      Write-Host '后端二进制构建失败' -ForegroundColor Red
      exit 1
    }
  }

  $backendLog = Join-Path $LogDir 'backend.log'
  $backendErr = Join-Path $LogDir 'backend.err.log'
  $backendEnv['HJZL_MCP_TRANSPORT'] = 'streamable-http'
  $backendEnv['HJZL_MCP_HOST'] = '127.0.0.1'
  $backendEnv['HJZL_MCP_PORT'] = "$BackendPort"
  $backendEnv['HJZL_MCP_CONFIG_FILE'] = $policyFile
  $backendEnv['HJZL_MCP_REGISTRY_PATH'] = (Join-Path $DataDir 'registry.sqlite3')
  $backendEnv['HJZL_MCP_AUDIT_PATH'] = (Join-Path $DataDir 'audit.sqlite3')
  $backendEnv['HJZL_ADMIN_TOKENS'] = $AdminTokens

  $saved = @{}
  foreach ($k in $backendEnv.Keys) {
    $saved[$k] = [Environment]::GetEnvironmentVariable($k, 'Process')
    [Environment]::SetEnvironmentVariable($k, $backendEnv[$k], 'Process')
  }
  try {
    $backendProc = Start-Process -FilePath $serverBin -WorkingDirectory $RepoRoot `
      -RedirectStandardOutput $backendLog -RedirectStandardError $backendErr -PassThru -NoNewWindow
  } finally {
    foreach ($k in $backendEnv.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k], 'Process') }
  }
  [void]$script:Children.Add([pscustomobject]@{ Name = 'backend'; Id = $backendProc.Id })
  Write-Host ("==> 后端已启动（pid {0}）：http://127.0.0.1:{1}" -f $backendProc.Id, $BackendPort)

  # 等 /health（最多 30s）
  $healthBody = Wait-Http "http://127.0.0.1:$BackendPort/health" 30 $curl
  if ($null -eq $healthBody) {
    Write-Host "后端未在 30s 内就绪（端口 $BackendPort 是否被占用？）" -ForegroundColor Red
    Write-Host '---- backend.log 尾部 ----'
    if (Test-Path $backendLog) { Get-Content $backendLog -Tail 30 | ForEach-Object { Write-Host $_ } }
    Write-Host '----------------------'
    Stop-Children
    exit 1
  }
  $chain = 'unknown'
  try { $chain = ($healthBody | ConvertFrom-Json).audit_chain } catch { $chain = 'unknown' }
  if ($chain -ne 'valid') {
    Write-Host ("警告：audit_chain = {0}（审计链异常），服务仍继续运行，请检查 .dev-data/audit.sqlite3" -f $chain) -ForegroundColor Yellow
  }

  # ------------------------------------------------------------ 前端
  $webPortUsed = 0
  if (-not $NoWeb) {
    $candidate = $WebPort
    while ($candidate -lt ($WebPort + 20) -and -not (Test-PortFree $candidate)) { $candidate++ }
    if ($candidate -ge ($WebPort + 20)) {
      Write-Host ("找不到可用前端端口（{0}-{1}）" -f $WebPort, ($WebPort + 19)) -ForegroundColor Red
      Stop-Children
      exit 1
    }
    if ($candidate -ne $WebPort) {
      Write-Host ("端口 {0} 被占用，前端改用 {1}" -f $WebPort, $candidate)
    }
    $webPortUsed = $candidate

    # 后端端口非 8000 时，admin-web 的代理目标需要改指向；不改动前端源码，
    # 而是在 .dev-data 生成一份只改代理目标的配置（复用 admin-web/vite.config.ts 的其余配置）。
    $viteArgs = @('run', 'dev', '--', '--host', '127.0.0.1', '--port', "$webPortUsed")
    if ($BackendPort -ne 8000) {
      $link = Join-Path $DataDir 'node_modules'
      if (-not (Test-Path $link)) {
        try {
          New-Item -ItemType Junction -Path $link -Target (Join-Path $WebDir 'node_modules') -ErrorAction Stop | Out-Null
        } catch {
          Write-Host ("警告：无法建立 .dev-data/node_modules 链接（{0}），管理台代理将仍指向 8000" -f $_.Exception.Message) -ForegroundColor Yellow
        }
      }
      $viteCfg = Join-Path $DataDir 'vite.config.local.mjs'
      $cfgText = @"
// 由 scripts/dev.ps1 生成：只改管理面代理的后端地址，其余配置复用 admin-web/vite.config.ts。
import base from '../admin-web/vite.config.ts'

const target = 'http://127.0.0.1:$BackendPort'
const resolved = typeof base === 'function' ? await base({ command: 'serve', mode: 'development', ssrBuild: false }) : base
const proxy = { ...(resolved.server?.proxy ?? {}) }
for (const key of Object.keys(proxy)) {
  proxy[key] = { ...proxy[key], target }
}
export default { ...resolved, server: { ...resolved.server, proxy } }
"@
      Set-Content -Path $viteCfg -Value $cfgText -Encoding utf8
      $viteArgs += @('--config', $viteCfg)
      Write-Host ("==> 管理面代理已指向后端端口 {0}（配置：{1}）" -f $BackendPort, $viteCfg)
    }

    $webLog = Join-Path $LogDir 'web.log'
    $webErr = Join-Path $LogDir 'web.err.log'
    $savedToken = [Environment]::GetEnvironmentVariable('HJZL_DEV_ADMIN_TOKEN', 'Process')
    [Environment]::SetEnvironmentVariable('HJZL_DEV_ADMIN_TOKEN', $AliceToken, 'Process')
    try {
      if ($env:OS -eq 'Windows_NT') {
        $webProc = Start-Process -FilePath 'cmd.exe' -ArgumentList (@('/c', 'npm') + $viteArgs) `
          -WorkingDirectory $WebDir -RedirectStandardOutput $webLog -RedirectStandardError $webErr -PassThru -NoNewWindow
      } else {
        $webProc = Start-Process -FilePath 'npm' -ArgumentList $viteArgs `
          -WorkingDirectory $WebDir -RedirectStandardOutput $webLog -RedirectStandardError $webErr -PassThru -NoNewWindow
      }
    } finally {
      [Environment]::SetEnvironmentVariable('HJZL_DEV_ADMIN_TOKEN', $savedToken, 'Process')
    }
    [void]$script:Children.Add([pscustomobject]@{ Name = 'web'; Id = $webProc.Id })
    Write-Host ("==> 前端已启动（pid {0}）：http://127.0.0.1:{1}" -f $webProc.Id, $webPortUsed)

    $index = Wait-Http "http://127.0.0.1:$webPortUsed/" 20 $curl
    if ($null -eq $index) {
      Write-Host '前端未在 20s 内就绪' -ForegroundColor Red
      Write-Host '---- web.log 尾部 ----'
      if (Test-Path $webLog) { Get-Content $webLog -Tail 30 | ForEach-Object { Write-Host $_ } }
      Write-Host '---------------------'
      Stop-Children
      exit 1
    }
  }

  # ------------------------------------------------------------ 横幅
  $webLine = "管理台   http://127.0.0.1:$webPortUsed"
  if ($NoWeb) { $webLine = '管理台   （未启动：-NoWeb）' }
  Write-Host ''
  Write-Host '================ 开发环境已就绪 ================'
  Write-Host $webLine
  Write-Host "MCP      http://127.0.0.1:$BackendPort/mcp"
  Write-Host "健康     http://127.0.0.1:$BackendPort/health"
  Write-Host "令牌     alice=$AliceToken （普通）  bob=$BobToken （审批）"
  Write-Host "数据     .dev-data/{registry,audit}.sqlite3   日志  .dev-data/logs/"
  Write-Host '停止     Ctrl+C（会同时停掉前后端）'
  Write-Host '================================================='
  Write-Host ''

  # ------------------------------------------------------------ 前台等待
  while ($true) {
    Start-Sleep -Milliseconds 500
    $dead = @()
    foreach ($c in $script:Children) {
      $p = Get-Process -Id $c.Id -ErrorAction SilentlyContinue
      if ($null -eq $p) { $dead += $c.Name }
    }
    if ($dead.Count -gt 0) {
      Write-Host ("进程已退出：{0}" -f ($dead -join ', ')) -ForegroundColor Yellow
      break
    }
  }
} finally {
  Stop-Children
}
