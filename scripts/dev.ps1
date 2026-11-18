<#
.SYNOPSIS
  一键启动本地开发环境：后端（streamable-http）+ 前端（Vite dev）。
.DESCRIPTION
  用法：pwsh scripts/dev.ps1 [-BackendPort 8000] [-AdminPort 8080] [-WebPort 5173] [-ClickHouseDsn <dsn>] [-ResetData] [-NoWeb]
    -BackendPort  后端 MCP 面端口（默认 8000）
    -AdminPort    后端管理面端口（默认 8080，必须与 MCP 面不同）
    -WebPort      前端 Vite 端口（默认 5173，被占用时自动 +1 顺延）
    -ClickHouseDsn 可选。给了就生成 .dev-data/policy.toml（追加 dev-clickhouse 数据源）并注入 JUMCP_DEV_CLICKHOUSE_DSN；
                  形如 http://<user>:<password>@<host>:<port>，凭证只经环境变量传递
    -ResetData    启动前清空 .dev-data/（默认保留，便于连续试用）
    -NoWeb        只起后端
  两个监听面（不能相同）：MCP 面挂 `/mcp`、`/internal/mcp/*`、`/health`；管理面挂 `/admin/*` 与管理台静态站。
  管理台自带登录页：浏览器打开前端地址，在登录页里输入管理面 token（本脚本的 alice/bob）即可，前端不读任何 token 环境变量。
  前端代理目标：管理面走 JUMCP_DEV_ADMIN_TARGET（默认 http://127.0.0.1:8080），MCP 面走 JUMCP_DEV_API_TARGET（默认 http://127.0.0.1:8000），
  两者都只存在于 Node 进程环境。
  Ctrl+C（或关窗）会同时停掉前后端子进程；日志在 .dev-data/logs/。
#>
[CmdletBinding()]
param(
  [int]$BackendPort = 8000,
  [int]$AdminPort = 8080,
  [int]$WebPort = 5173,
  [string]$ClickHouseDsn = '',
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
# /mcp 的 per-request 身份：agent_id:role:token（role 必须在 config/policy.toml 的 [roles] 里）
$AgentTokens = 'agent-analyst:analyst:tok_agent_analyst_0001,agent-admin:admin:tok_agent_admin_000001'
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

# 监听地址：后端跟随 JUMCP_MCP_HOST（默认 0.0.0.0），前端固定 0.0.0.0（本机访问仍用 127.0.0.1）
$BackendHost = if ($env:JUMCP_MCP_HOST) { $env:JUMCP_MCP_HOST } else { '0.0.0.0' }
$WebHost = '0.0.0.0'

function Test-PortFree {
  # 端口在 $BindHost 上未被占用返回 $true（node 试绑一下；必须按实际监听地址探测，否则会漏判通配绑定）
  param([int]$Port, [string]$BindHost = '0.0.0.0')
  & node -e 'const net=require("net");const s=net.createServer();s.once("error",()=>process.exit(1));s.listen(Number(process.argv[1]),process.argv[2],()=>s.close(()=>process.exit(0)))' $Port $BindHost | Out-Null
  return ($LASTEXITCODE -eq 0)
}

function Test-PortListening {
  # 端口已有监听者返回 $true。Windows 优先 Get-NetTCPConnection，其次 Test-NetConnection，最后 node 试绑
  param([int]$Port, [string]$BindHost = '0.0.0.0')
  if ($env:OS -eq 'Windows_NT') {
    if (Get-Command -Name Get-NetTCPConnection -ErrorAction SilentlyContinue) {
      return (@(Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue).Count -gt 0)
    }
    if (Get-Command -Name Test-NetConnection -ErrorAction SilentlyContinue) {
      try { return [bool](Test-NetConnection -ComputerName 127.0.0.1 -Port $Port -InformationLevel Quiet -ErrorAction Stop) } catch { }
    }
  }
  return (-not (Test-PortFree $Port $BindHost))
}

function Get-PortOwnerPids {
  # 取出监听该端口的进程 pid（仅 Windows 的 Get-NetTCPConnection 能给出）
  param([int]$Port)
  if ($env:OS -ne 'Windows_NT') { return @() }
  return @(Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue |
    Select-Object -ExpandProperty OwningProcess -Unique | Where-Object { $_ -and $_ -ne 0 })
}

function Show-LogTail {
  param([string]$Path, [int]$Lines = 30)
  if (Test-Path $Path) { Get-Content $Path -Tail $Lines -ErrorAction SilentlyContinue | ForEach-Object { Write-Host $_ } }
}

function Wait-Http {
  # 等待 URL 在 $Seconds 秒内返回 200；成功返回响应体，失败返回 $null。
  # 给了 -Proc 时，一旦该进程退出立即返回 $null（避免把别人的服务当成自己的）
  param([string]$Url, [int]$Seconds, [string]$Curl, [System.Diagnostics.Process]$Proc)
  $deadline = (Get-Date).AddSeconds($Seconds)
  while ((Get-Date) -lt $deadline) {
    if ($null -ne $Proc -and $Proc.HasExited) { return $null }
    $body = Join-Path $env:TEMP ("jumcp-dev-" + [System.Guid]::NewGuid().ToString('N').Substring(0, 6) + '.txt')
    $code = (& $Curl -sS --connect-timeout 2 --max-time 3 -o $body -w '%{http_code}' $Url 2>$null | Out-String).Trim()
    if ($code -eq '200') {
      # 命中前再确认进程还活着：进程已死说明响应来自别的进程
      if ($null -ne $Proc -and $Proc.HasExited) { Remove-Item $body -Force -ErrorAction SilentlyContinue; return $null }
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
  # 参数校验：单一主数据源
  # 后端启动时只接受一个 readonly 数据源，这里不需要互斥判定；保留 try 结构便于收尾。

  # 准备数据目录
  if ($ResetData -and (Test-Path $DataDir)) {
    Write-Host "==> 清空 $DataDir"
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

  # 配置：可选 ClickHouse 数据源
  $policyFile = $BasePolicy
  $backendEnv = @{}
  if ($ClickHouseDsn -ne '') {
    $policyFile = Join-Path $DataDir 'policy.toml'
    Copy-Item $BasePolicy $policyFile -Force
    $dsnBlock = @"

[[datasources]]
name = "dev-clickhouse"
engine = "clickhouse"
readonly = true
allowed_schemas = []
statement_timeout_ms = 15000
dsn_env = "JUMCP_DEV_CLICKHOUSE_DSN"
"@
    Add-Content -Path $policyFile -Value $dsnBlock -Encoding utf8
    $backendEnv['JUMCP_DEV_CLICKHOUSE_DSN'] = $ClickHouseDsn
    Write-Host "==> 已生成临时数据源配置：$policyFile（数据源 dev-clickhouse，DSN 走 JUMCP_DEV_CLICKHOUSE_DSN）"
  } else {
    Write-Host '==> 未提供 -ClickHouseDsn：使用 config/policy.toml 原样（内置查询工具不接 ClickHouse，ClickHouse 需通过上游 MCP 接入）'
  }

  # 后端
  # 1) 端口守卫：端口已有监听者说明多半是上一次遗留的旧后端；它会让新进程 bind 失败，
  #    而 /health 又会命中旧进程，从而被误判为“就绪”。这里直接失败，不自动杀进程。
  if ($AdminPort -eq $BackendPort) {
    Write-Host ("错误：-AdminPort 与 -BackendPort 相同（{0}）：管理面与 MCP 面必须使用不同端口（后端会拒绝启动）。" -f $AdminPort) -ForegroundColor Red
    exit 1
  }
  foreach ($guardPort in @($BackendPort, $AdminPort)) {
    if (-not (Test-PortListening $guardPort $BackendHost)) { continue }
    Write-Host ("错误：后端端口 {0} 已被占用，疑似上一次遗留的旧后端仍在运行（继续启动会被误判为就绪）。" -f $guardPort) -ForegroundColor Red
    $owners = @(Get-PortOwnerPids $guardPort)
    if ($owners.Count -gt 0) {
      Write-Host ("监听该端口的 pid：{0}（Get-Process -Id <pid> 查看详情）" -f ($owners -join ', '))
    }
    if ($env:OS -eq 'Windows_NT') {
      Write-Host ("排查：netstat -ano | findstr :{0}    然后  Get-Process -Id <pid>" -f $guardPort)
    } else {
      Write-Host ("排查：lsof -i :{0}   或   ss -lptn（再 kill <pid>）" -f $guardPort)
    }
    Write-Host '请先停止旧后端，或改用 -BackendPort / -AdminPort <其他端口>。脚本不会自动杀进程。'
    exit 1
  }

  # 2) 每次启动都增量构建：二进制存在不代表是最新代码
  $exeName = 'jumcp-server'
  if ($env:OS -eq 'Windows_NT') { $exeName = 'jumcp-server.exe' }
  $serverBin = Join-Path $BackendDir ("target/debug/$exeName")
  Write-Host '==> cargo build --bin jumcp-server --bin jumcp-connector-clickhouse（增量构建，避免运行过期二进制）'
  Push-Location $BackendDir
  $buildCode = 1
  try { & cargo build --bin jumcp-server --bin jumcp-connector-clickhouse; $buildCode = $LASTEXITCODE } finally { Pop-Location }
  if ($buildCode -ne 0) {
    Write-Host ("后端构建失败（cargo build 退出码 {0}）" -f $buildCode) -ForegroundColor Red
    exit 1
  }
  if (-not (Test-Path $serverBin)) {
    Write-Host ("构建后仍找不到后端二进制：{0}" -f $serverBin) -ForegroundColor Red
    exit 1
  }

  $backendLog = Join-Path $LogDir 'backend.log'
  $backendErr = Join-Path $LogDir 'backend.err.log'
  $backendEnv['JUMCP_MCP_TRANSPORT'] = 'streamable-http'
  $backendEnv['JUMCP_MCP_HOST'] = $BackendHost
  $backendEnv['JUMCP_MCP_PORT'] = "$BackendPort"
  $backendEnv['JUMCP_ADMIN_PORT'] = "$AdminPort"
  $backendEnv['JUMCP_MCP_CONFIG_FILE'] = $policyFile
  $backendEnv['JUMCP_MCP_REGISTRY_PATH'] = (Join-Path $DataDir 'registry.sqlite3')
  $backendEnv['JUMCP_MCP_AUDIT_PATH'] = (Join-Path $DataDir 'audit.sqlite3')
  $backendEnv['JUMCP_ADMIN_TOKENS'] = $AdminTokens
  $backendEnv['JUMCP_AGENT_TOKENS'] = $AgentTokens
  # 本地开发用固定密钥：HTTP 模式缺它会拒绝启动（密码字段加密用）。
  $backendEnv['JUMCP_SECRET_KEY'] = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
  # 传输密钥（与落库密钥用途分离）：管理台在浏览器里用它加密 secret 字段。
  # 固定值仅供本地开发，生产必须用 `openssl rand -hex 32` 生成并通过 secret 注入。
  $backendEnv['JUMCP_TRANSPORT_KEY'] = 'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd'

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

  # 等两个面的 /health（各最多 30s）；进程一退出立即判失败，避免把别人的 /health 当成就绪。
  # 只探一个面时"另一面没监听"会被当成启动成功。
  $healthBody = Wait-Http "http://127.0.0.1:$BackendPort/health" 30 $curl -Proc $backendProc
  $adminHealthBody = $null
  if ($null -ne $healthBody -and -not $backendProc.HasExited) {
    $adminHealthBody = Wait-Http "http://127.0.0.1:$AdminPort/health" 30 $curl -Proc $backendProc
  }
  if ($backendProc.HasExited) {
    Write-Host ("后端进程已退出（exit code {0}），多半是端口 {1}/{2} 在启动瞬间被抢占或配置非法。" -f $backendProc.ExitCode, $BackendPort, $AdminPort) -ForegroundColor Red
    Write-Host '---- backend.err.log 尾部 ----'
    Show-LogTail $backendErr
    Write-Host '---- backend.log 尾部 ----'
    Show-LogTail $backendLog
    Write-Host '----------------------'
    Stop-Children
    exit 1
  }
  if ($null -eq $adminHealthBody) {
    Write-Host ("后端未在 30s 内就绪（MCP 面 $BackendPort / 管理面 $AdminPort 是否被占用？）") -ForegroundColor Red
    Write-Host '---- backend.log 尾部 ----'
    Show-LogTail $backendLog
    Write-Host '---- backend.err.log 尾部 ----'
    Show-LogTail $backendErr
    Write-Host '----------------------'
    Stop-Children
    exit 1
  }
  $chain = 'unknown'
  try { $chain = ($healthBody | ConvertFrom-Json).audit_chain } catch { $chain = 'unknown' }
  if ($chain -ne 'valid') {
    Write-Host ("警告：audit_chain = {0}（审计链异常），服务仍继续运行，请检查 .dev-data/audit.sqlite3" -f $chain) -ForegroundColor Yellow
  }

  # 前端
  $webPortUsed = 0
  if (-not $NoWeb) {
    $candidate = $WebPort
    while ($candidate -lt ($WebPort + 20) -and -not (Test-PortFree $candidate $WebHost)) { $candidate++ }
    if ($candidate -ge ($WebPort + 20)) {
      Write-Host ("找不到可用前端端口（{0}-{1}）" -f $WebPort, ($WebPort + 19)) -ForegroundColor Red
      Stop-Children
      exit 1
    }
    if ($candidate -ne $WebPort) {
      Write-Host ("端口 {0} 被占用，前端改用 {1}" -f $WebPort, $candidate)
    }
    $webPortUsed = $candidate

    # 前端代理目标：管理面 / MCP 面分开（双端口），只存在于 Node 进程环境
    # 监听 0.0.0.0（本机访问用 127.0.0.1，容器/WSL/局域网用宿主 IP）
    $viteArgs = @('run', 'dev', '--', '--host', $WebHost, '--port', "$webPortUsed")
    $apiTarget = "http://127.0.0.1:$BackendPort"
    $adminTarget = "http://127.0.0.1:$AdminPort"
    Write-Host ("==> 前端代理目标：管理面 {0} ，MCP 面 {1}" -f $adminTarget, $apiTarget)

    $webLog = Join-Path $LogDir 'web.log'
    $webErr = Join-Path $LogDir 'web.err.log'
    $savedTarget = [Environment]::GetEnvironmentVariable('JUMCP_DEV_API_TARGET', 'Process')
    $savedAdminTarget = [Environment]::GetEnvironmentVariable('JUMCP_DEV_ADMIN_TARGET', 'Process')
    [Environment]::SetEnvironmentVariable('JUMCP_DEV_API_TARGET', $apiTarget, 'Process')
    [Environment]::SetEnvironmentVariable('JUMCP_DEV_ADMIN_TARGET', $adminTarget, 'Process')
    try {
      if ($env:OS -eq 'Windows_NT') {
        $webProc = Start-Process -FilePath 'cmd.exe' -ArgumentList (@('/c', 'npm') + $viteArgs) `
          -WorkingDirectory $WebDir -RedirectStandardOutput $webLog -RedirectStandardError $webErr -PassThru -NoNewWindow
      } else {
        $webProc = Start-Process -FilePath 'npm' -ArgumentList $viteArgs `
          -WorkingDirectory $WebDir -RedirectStandardOutput $webLog -RedirectStandardError $webErr -PassThru -NoNewWindow
      }
    } finally {
      [Environment]::SetEnvironmentVariable('JUMCP_DEV_API_TARGET', $savedTarget, 'Process')
      [Environment]::SetEnvironmentVariable('JUMCP_DEV_ADMIN_TARGET', $savedAdminTarget, 'Process')
    }
    [void]$script:Children.Add([pscustomobject]@{ Name = 'web'; Id = $webProc.Id })
    Write-Host ("==> 前端已启动（pid {0}）：监听 0.0.0.0:{1}，本机访问 http://127.0.0.1:{1}" -f $webProc.Id, $webPortUsed)

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

  # 横幅
  $webLine = "管理台   http://127.0.0.1:$webPortUsed （监听 0.0.0.0:$webPortUsed）"
  if ($NoWeb) { $webLine = '管理台   （未启动：-NoWeb）' }
  Write-Host ''
  Write-Host '================ 开发环境已就绪 ================'
  Write-Host $webLine
  Write-Host '登录     打开上面的管理台地址 → 在登录页输入管理面 token（alice 或 bob）'
  Write-Host "MCP      http://127.0.0.1:$BackendPort/mcp"
  Write-Host "管理面   http://127.0.0.1:$AdminPort/ （登录页；脚本直连 /admin/* 需 Bearer）"
  Write-Host "健康     http://127.0.0.1:$BackendPort/health （MCP 面） / http://127.0.0.1:$AdminPort/health （管理面）"
  Write-Host "令牌     alice=$AliceToken （普通）  bob=$BobToken （审批）"
  Write-Host "数据     .dev-data/{registry,audit}.sqlite3   日志  .dev-data/logs/"
  Write-Host '停止     Ctrl+C（会同时停掉前后端）'
  Write-Host '================================================='
  Write-Host ''

  # 前台等待
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
