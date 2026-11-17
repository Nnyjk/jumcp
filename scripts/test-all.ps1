<#
.SYNOPSIS
  一键本地测试：后端静态检查 + 单元/契约测试 + 前端类型检查/构建/lint + 端到端冒烟。
.DESCRIPTION
  用法：pwsh scripts/test-all.ps1 [-SkipE2E] [-ClickHouseDsn <dsn>] [-NoClean]
    -SkipE2E   只跑静态检查与测试，不做端到端冒烟
    -ClickHouseDsn 设置 JUMCP_TEST_CLICKHOUSE_DSN，并额外跑 #[ignore] 的真库用例
    -NoClean   保留临时数据目录（含服务日志）便于排查
  退出码：0 = 全部通过；1 = 有步骤失败；2 = 参数错误。
  要求 PowerShell 7+；检测到 5.1 时会明确提示并尽量兼容运行。
#>
[CmdletBinding()]
param(
  [switch]$SkipE2E,
  [string]$ClickHouseDsn,
  [switch]$NoClean,
  [switch]$Help
)

$ErrorActionPreference = 'Continue'
Set-StrictMode -Version 2.0
$global:LASTEXITCODE = 0

if ($Help) {
  Get-Help $PSCommandPath -Full
  exit 0
}

if ($PSVersionTable.PSVersion.Major -lt 7) {
  Write-Warning ("检测到 PowerShell {0}，本脚本要求 7+。已尽力以 5.1 兼容模式运行；" -f $PSVersionTable.PSVersion) +
    "若出现语法或参数错误，请安装 PowerShell 7：winget install Microsoft.PowerShell"
}

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Split-Path -Parent $ScriptDir
$BackendDir = Join-Path $RepoRoot 'backend'
$WebDir = Join-Path $RepoRoot 'admin-web'
$PolicyFile = Join-Path $RepoRoot 'config/policy.toml'

$AdminTokens = 'alice:tok_alice_000000000,bob:tok_bob_00000000000'
$AliceToken = 'tok_alice_000000000'
# /mcp 的 per-request 身份（agent_id:role:token）；E2E 用 analyst 主体取工具清单
$AgentTokens = 'agent-analyst:analyst:tok_agent_analyst_0001,agent-admin:admin:tok_agent_admin_000001'
$AnalystAgentToken = 'tok_agent_analyst_0001'
# E2E 访问地址；默认 127.0.0.1，可用 JUMCP_TEST_E2E_HOST 覆盖（如 WSL 里访问 Windows 宿主）
$E2EHost = '127.0.0.1'
if ($env:JUMCP_TEST_E2E_HOST) { $E2EHost = $env:JUMCP_TEST_E2E_HOST }
# 监听地址；默认 0.0.0.0，可用 JUMCP_TEST_BIND_HOST 覆盖
$BindHost = '0.0.0.0'
if ($env:JUMCP_TEST_BIND_HOST) { $BindHost = $env:JUMCP_TEST_BIND_HOST }

$script:Steps = New-Object System.Collections.ArrayList
$script:PassCount = 0
$script:FailCount = 0
$script:TempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("jumcp-testall-" + [System.Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Force -Path $script:TempDir | Out-Null
$script:ServerProc = $null
$script:CheckLog = Join-Path $script:TempDir 'checks.log'

function Get-LogPath {
  param([string]$Name)
  $safe = ($Name -replace '[^A-Za-z0-9]', '_')
  return (Join-Path $script:TempDir ($safe + '.log'))
}

function Add-Step {
  param([string]$Name, [string]$Status, [double]$Seconds, [string]$Log)
  [void]$script:Steps.Add([pscustomobject]@{
    Name = $Name; Status = $Status; Seconds = $Seconds; Log = $Log
  })
  if ($Status -eq 'PASS') { $script:PassCount++ } else { $script:FailCount++ }
}

function Write-Summary {
  Write-Host ''
  Write-Host '================ 汇总 ================'
  Write-Host ("{0,-46} {1,-6} {2,8}" -f 'STEP', 'STATUS', 'SECONDS')
  Write-Host ("{0,-46} {1,-6} {2,8}" -f ('-' * 46), ('-' * 6), ('-' * 8))
  foreach ($s in $script:Steps) {
    Write-Host ("{0,-46} {1,-6} {2,8:N1}" -f $s.Name, $s.Status, $s.Seconds)
  }
  Write-Host ("{0,-46} {1,-6} {2,8}" -f ('-' * 46), ('-' * 6), ('-' * 8))
  Write-Host ("Total: {0} passed, {1} failed" -f $script:PassCount, $script:FailCount)
}

function Show-LogTail {
  param([string]$Log, [int]$Lines = 40)
  if ($Log -and (Test-Path $Log)) {
    Write-Host ("---- 输出尾部 ----")
    Get-Content $Log -Tail $Lines -ErrorAction SilentlyContinue | ForEach-Object { Write-Host $_ }
    Write-Host '------------------'
  }
}

function Stop-Server {
  if ($null -ne $script:ServerProc) {
    try {
      if (-not $script:ServerProc.HasExited) {
        Stop-Process -Id $script:ServerProc.Id -Force -ErrorAction SilentlyContinue
        $script:ServerProc.WaitForExit(5000) | Out-Null
      }
    } catch { }
    $script:ServerProc = $null
  }
}

function Remove-Temp {
  if ($NoClean) {
    Write-Host ("临时目录已保留：{0}" -f $script:TempDir)
  } else {
    Remove-Item -Recurse -Force $script:TempDir -ErrorAction SilentlyContinue
  }
}

function Invoke-Step {
  # Invoke-Step <name> <workdir> <scriptblock>；失败即打印日志尾部 + 汇总并退出 1
  param([string]$Name, [string]$WorkDir, [scriptblock]$Command)
  Write-Host ("==> {0}" -f $Name)
  $log = Get-LogPath $Name
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  Push-Location $WorkDir
  try {
    & $Command *>&1 | Out-File -FilePath $log -Encoding utf8
    $code = $LASTEXITCODE
  } finally {
    Pop-Location
  }
  $sw.Stop()
  if ($null -eq $code) { $code = 0 }
  if ($code -eq 0) {
    Add-Step $Name 'PASS' $sw.Elapsed.TotalSeconds $log
    return $true
  }
  Add-Step $Name 'FAIL' $sw.Elapsed.TotalSeconds $log
  Show-LogTail $log
  Write-Summary
  Stop-Server
  Remove-Temp
  exit 1
}

function Fail-E2E {
  # Fail-E2E <name> <seconds> <log> <message>
  param([string]$Name, [double]$Seconds, [string]$Log, [string]$Message)
  if ($Message) { Write-Host $Message }
  Add-Step $Name 'FAIL' $Seconds $Log
  Stop-Server
  Show-LogTail $Log
  Write-Summary
  Remove-Temp
  exit 1
}

function Invoke-NodeCheck {
  # Invoke-NodeCheck <file> <js>：JS 里用 d 表示解析后的 JSON；成功返回 $true
  param([string]$File, [string]$Js)
  & node -e $Js $File *>> $script:CheckLog
  return ($LASTEXITCODE -eq 0)
}

function Resolve-Tool {
  # Resolve-Tool <name> <候选可执行名...>；返回路径或 $null
  param([string]$Name, [string[]]$Candidates)
  foreach ($c in $Candidates) {
    $found = @(Get-Command -Name $c -CommandType Application -ErrorAction SilentlyContinue)
    if ($found.Count -gt 0) { return $found[0].Source }
  }
  return $null
}

try {
  # 1. 环境探测
  Write-Host '==> env: toolchain probe'
  $envLog = Get-LogPath 'env: toolchain probe'
  $toolCandidates = @{
    'cargo' = @('cargo', 'cargo.exe')
    'node'  = @('node', 'node.exe')
    'npm'   = @('npm.cmd', 'npm.exe', 'npm')
    'curl'  = @('curl.exe', 'curl')
  }
  $resolved = @{}
  $missing = @()
  foreach ($t in $toolCandidates.Keys) {
    $path = Resolve-Tool $t $toolCandidates[$t]
    if ($null -eq $path) { $missing += $t } else { $resolved[$t] = $path }
  }
  $envLines = @("cwd: $RepoRoot")
  foreach ($t in @('cargo', 'node', 'npm', 'curl')) {
    if ($resolved.ContainsKey($t)) { $envLines += ("{0}: {1}" -f $t, $resolved[$t]) } else { $envLines += ("{0}: MISSING" -f $t) }
  }
  if ($missing.Count -gt 0) {
    $envLines += ''
    $envLines += ("缺少必需工具：{0}" -f ($missing -join ' '))
    $envLines += '安装指引：'
    $envLines += '  cargo → Rust 工具链：https://rustup.rs （或 apt/brew install rust）'
    $envLines += '  node  → Node.js LTS 18+：https://nodejs.org （或 apt/brew install nodejs）'
    $envLines += '  npm   → 随 Node.js 一并安装；确认 npm -v 可用'
    $envLines += '  curl  → macOS/Linux 自带；apt install curl / brew install curl'
    $envLines += '  若本机已装，请确认已加入 PATH。'
    $envLines | Out-File -FilePath $envLog -Encoding utf8
    Add-Step 'env: toolchain probe' 'FAIL' 0 $envLog
    $envLines | ForEach-Object { Write-Host $_ }
    Write-Summary
    Remove-Temp
    exit 1
  }
  $envLines | Out-File -FilePath $envLog -Encoding utf8
  Add-Step 'env: toolchain probe' 'PASS' 0 $envLog
  $envLines | ForEach-Object { Write-Host $_ }

  # 2. 后端
  Invoke-Step 'backend: cargo fmt --check' $BackendDir { & cargo fmt --check } | Out-Null
  Invoke-Step 'backend: cargo clippy -D warnings' $BackendDir { & cargo clippy --all-targets -- -D warnings } | Out-Null
  Invoke-Step 'backend: cargo test --workspace' $BackendDir { & cargo test --workspace } | Out-Null
  if ($ClickHouseDsn) {
    $env:JUMCP_TEST_CLICKHOUSE_DSN = $ClickHouseDsn
    Invoke-Step 'backend: build connector agent' $BackendDir { & cargo build -p jumcp-connector-clickhouse } | Out-Null
    Invoke-Step 'backend: cargo test --ignored (真库)' $BackendDir { & cargo test --workspace -- --ignored } | Out-Null
  }

  # 3. 前端
  Invoke-Step 'frontend: npm ci' $WebDir { & npm ci } | Out-Null
  Invoke-Step 'frontend: npm run typecheck' $WebDir { & npm run typecheck } | Out-Null
  Invoke-Step 'frontend: npm run build' $WebDir { & npm run build } | Out-Null
  Invoke-Step 'frontend: eslint src --max-warnings 0' $WebDir { & npx eslint src --max-warnings 0 } | Out-Null

  # 4. E2E 冒烟
  if ($SkipE2E) {
    Write-Host '跳过 E2E 冒烟（-SkipE2E）'
    Write-Summary
    exit 0
  }

  Invoke-Step 'e2e: build jumcp-server' $BackendDir { & cargo build --bin jumcp-server } | Out-Null

  $exeName = 'jumcp-server'
  if ($env:OS -eq 'Windows_NT') { $exeName = 'jumcp-server.exe' }
  $serverBin = Join-Path $BackendDir ("target/debug/$exeName")
  if (-not (Test-Path $serverBin)) {
    Add-Step 'e2e: 启动服务' 'FAIL' 0 ''
    Write-Host ("未找到服务二进制：{0}" -f $serverBin)
    Write-Summary
    Remove-Temp
    exit 1
  }

  # 顺序探测空闲端口（不使用固定端口）
  $port = $null
  for ($p = 18100; $p -lt 18200; $p++) {
    & node -e 'const net=require("net");const s=net.createServer();s.once("error",()=>process.exit(1));s.listen(Number(process.argv[1]),process.argv[2],()=>s.close(()=>process.exit(0)))' $p $BindHost | Out-Null
    if ($LASTEXITCODE -eq 0) { $port = $p; break }
  }
  if ($null -eq $port) {
    Add-Step 'e2e: 选端口' 'FAIL' 0 ''
    Write-Host '18100-18199 范围内没有空闲端口'
    Write-Summary
    Remove-Temp
    exit 1
  }

  Write-Host ("==> e2e: 启动服务（监听 {0}:{1}，E2E 访问 http://{2}:{1}）" -f $BindHost, $port, $E2EHost)
  $serverLog = Join-Path $script:TempDir 'server.log'
  $errLog = Join-Path $script:TempDir 'server.err.log'
  $saved = @{}
  $vars = @{
    JUMCP_MCP_TRANSPORT      = 'streamable-http'
    JUMCP_MCP_HOST          = $BindHost
    JUMCP_MCP_PORT          = "$port"
    JUMCP_MCP_CONFIG_FILE   = $PolicyFile
    JUMCP_MCP_REGISTRY_PATH = (Join-Path $script:TempDir 'registry.sqlite3')
    JUMCP_MCP_AUDIT_PATH    = (Join-Path $script:TempDir 'audit.sqlite3')
    JUMCP_ADMIN_TOKENS      = $AdminTokens
    JUMCP_AGENT_TOKENS      = $AgentTokens
  }
  foreach ($k in $vars.Keys) {
    $saved[$k] = [Environment]::GetEnvironmentVariable($k, 'Process')
    [Environment]::SetEnvironmentVariable($k, $vars[$k], 'Process')
  }
  try {
    $script:ServerProc = Start-Process -FilePath $serverBin -WorkingDirectory $RepoRoot `
      -RedirectStandardOutput $serverLog -RedirectStandardError $errLog -PassThru -NoNewWindow
  } catch {
    foreach ($k in $vars.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k], 'Process') }
    Add-Step 'e2e: 启动服务' 'FAIL' 0 ''
    Write-Host ("启动失败：{0}" -f $_.Exception.Message)
    Write-Summary
    Remove-Temp
    exit 1
  }
  foreach ($k in $vars.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k], 'Process') }

  $curl = $resolved['curl']
  $base = "http://$E2EHost`:$port"

  # 4a. 健康检查：最多 30s
  Write-Host '==> e2e: health /health'
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $healthFile = Join-Path $script:TempDir 'health.json'
  $code = ''
  $deadline = (Get-Date).AddSeconds(30)
  while ((Get-Date) -lt $deadline) {
    $code = (& $curl -sS --connect-timeout 2 --max-time 3 -o $healthFile -w '%{http_code}' "$base/health" 2>$null | Out-String).Trim()
    if ($code -eq '200') { break }
    Start-Sleep -Seconds 1
  }
  if ($code -ne '200') {
    $sw.Stop()
    Fail-E2E 'e2e: health /health' $sw.Elapsed.TotalSeconds $serverLog ("服务在 30s 内未就绪（最后一次状态码：{0}）" -f $code)
  }
  $ok = Invoke-NodeCheck $healthFile 'const d=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));if(d.audit_chain!=="valid"){console.error("audit_chain="+JSON.stringify(d.audit_chain));process.exit(1)}'
  $sw.Stop()
  if (-not $ok) {
    Fail-E2E 'e2e: health /health' $sw.Elapsed.TotalSeconds $script:CheckLog 'health 响应 audit_chain 不是 valid'
  }
  Add-Step 'e2e: health /health' 'PASS' $sw.Elapsed.TotalSeconds $serverLog

  # 4b. 管理面鉴权：匿名 401 / alice 200 且含 items
  Write-Host '==> e2e: admin auth /admin/review-queue'
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $authFile = Join-Path $script:TempDir 'review_queue.json'
  $code = (& $curl -sS --connect-timeout 2 --max-time 5 -o $authFile -w '%{http_code}' "$base/admin/review-queue" 2>$null | Out-String).Trim()
  if ($code -ne '401') {
    $sw.Stop()
    Fail-E2E 'e2e: admin auth /admin/review-queue' $sw.Elapsed.TotalSeconds '' ("匿名访问 /admin/review-queue 期望 401，实际 {0}" -f $code)
  }
  $code = (& $curl -sS --connect-timeout 2 --max-time 5 -o $authFile -w '%{http_code}' -H "Authorization: Bearer $AliceToken" "$base/admin/review-queue" 2>$null | Out-String).Trim()
  if ($code -ne '200') {
    $sw.Stop()
    Fail-E2E 'e2e: admin auth /admin/review-queue' $sw.Elapsed.TotalSeconds '' ("带 alice token 访问 /admin/review-queue 期望 200，实际 {0}" -f $code)
  }
  $ok = Invoke-NodeCheck $authFile 'const d=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));if(!Array.isArray(d.items)){console.error("响应缺少 items 字段");process.exit(1)}'
  $sw.Stop()
  if (-not $ok) {
    Fail-E2E 'e2e: admin auth /admin/review-queue' $sw.Elapsed.TotalSeconds $script:CheckLog 'review-queue 响应缺少 items 字段'
  }
  Add-Step 'e2e: admin auth /admin/review-queue' 'PASS' $sw.Elapsed.TotalSeconds $serverLog

  # 4c. MCP：initialize（带 _meta.mcp.groups）→ tools/list
  Write-Host '==> e2e: mcp initialize + tools/list'
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $initFile = Join-Path $script:TempDir 'mcp_init.json'
  $toolsFile = Join-Path $script:TempDir 'mcp_tools.json'
  $initBody = '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"_meta":{"mcp":{"groups":["db.read.core"]}}}}'
  $code = (& $curl -sS --connect-timeout 2 --max-time 5 -o $initFile -w '%{http_code}' -X POST -H 'Content-Type: application/json' -H "Authorization: Bearer $AnalystAgentToken" --data $initBody "$base/mcp" 2>$null | Out-String).Trim()
  if ($code -ne '200') {
    $sw.Stop()
    Fail-E2E 'e2e: mcp initialize + tools/list' $sw.Elapsed.TotalSeconds '' ("POST /mcp initialize 期望 200，实际 {0}" -f $code)
  }
  $ok = Invoke-NodeCheck $initFile 'const d=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));if(!d.result||!d.result.serverInfo){console.error("initialize 响应缺少 result.serverInfo: "+JSON.stringify(d).slice(0,300));process.exit(1)}'
  if (-not $ok) {
    $sw.Stop()
    Fail-E2E 'e2e: mcp initialize + tools/list' $sw.Elapsed.TotalSeconds $script:CheckLog 'initialize 响应缺少 result.serverInfo'
  }
  $listBody = '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
  $code = (& $curl -sS --connect-timeout 2 --max-time 5 -o $toolsFile -w '%{http_code}' -X POST -H 'Content-Type: application/json' -H "Authorization: Bearer $AnalystAgentToken" --data $listBody "$base/mcp" 2>$null | Out-String).Trim()
  if ($code -ne '200') {
    $sw.Stop()
    Fail-E2E 'e2e: mcp initialize + tools/list' $sw.Elapsed.TotalSeconds '' ("POST /mcp tools/list 期望 200，实际 {0}" -f $code)
  }
  $ok = Invoke-NodeCheck $toolsFile 'const d=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));const names=((d.result||{}).tools||[]).map(t=>t.name);const want=["search_capabilities","describe_capability","list_groups"];const missing=want.filter(n=>!names.includes(n));if(missing.length){console.error("tools/list 缺少: "+missing.join(","));process.exit(1)}'
  $sw.Stop()
  if (-not $ok) {
    Fail-E2E 'e2e: mcp initialize + tools/list' $sw.Elapsed.TotalSeconds $script:CheckLog 'tools/list 工具清单不完整'
  }
  Add-Step 'e2e: mcp initialize + tools/list' 'PASS' $sw.Elapsed.TotalSeconds $serverLog

  # 4d. 审计事件
  Write-Host '==> e2e: admin /admin/audit-events'
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $auditFile = Join-Path $script:TempDir 'audit_events.json'
  $code = (& $curl -sS --connect-timeout 2 --max-time 5 -o $auditFile -w '%{http_code}' -H "Authorization: Bearer $AliceToken" "$base/admin/audit-events?limit=5" 2>$null | Out-String).Trim()
  if ($code -ne '200') {
    $sw.Stop()
    Fail-E2E 'e2e: admin /admin/audit-events' $sw.Elapsed.TotalSeconds '' ("GET /admin/audit-events 期望 200，实际 {0}" -f $code)
  }
  $ok = Invoke-NodeCheck $auditFile 'const d=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));if(typeof d.total!=="number"||d.total<1){console.error("audit-events total="+JSON.stringify(d.total));process.exit(1)}if(d.order!=="desc"){console.error("audit-events 默认 order 应为 desc，实际 "+JSON.stringify(d.order));process.exit(1)}const K=["action","approval","changes","correlation_id","data","error_code","error_message","from_status","object_id","object_type","outcome","reason","schema_version","to_status"].sort().join(",");const LEG=["diff","approval_id","capability_id","before_hash","after_hash","version"];for(const e of d.events||[]){const p=e.payload||{};if(p.schema_version!==1){console.error("payload.schema_version="+JSON.stringify(p.schema_version)+" event="+e.event_id);process.exit(1)}if(Object.keys(p).sort().join(",")!==K){console.error("payload 键集不是 14 键: "+Object.keys(p).join(","));process.exit(1)}if(LEG.some(k=>k in p)){console.error("payload 残留旧字段: "+e.event_id);process.exit(1)}if(!p.object_type||!p.object_id||!p.action||!p.outcome){console.error("四件套缺失: "+e.event_id);process.exit(1)}}'
  $sw.Stop()
  if (-not $ok) {
    Fail-E2E 'e2e: admin /admin/audit-events' $sw.Elapsed.TotalSeconds $script:CheckLog 'audit-events 断言失败：total<1 / 默认 order 非 desc / payload 非 14 键 / schema_version≠1 / 残留旧字段 / 四件套缺失'
  }
  Add-Step 'e2e: admin /admin/audit-events' 'PASS' $sw.Elapsed.TotalSeconds $serverLog

  # 4e. 平台 token：创建（仅 db.read.core + 两项内置工具）→ tools/list 过滤 → 撤销后拒绝
  Write-Host '==> e2e: admin token create + mcp tools/list filter'
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $tokenFile = Join-Path $script:TempDir 'token_create.json'
  $tokenBody = '{"name":"e2e-只读组","role":"analyst","groups":["db.read.core"],"builtin_tools":["search_capabilities","describe_capability"],"source":"e2e","caller":"e2e-agent","expires_at":null}'
  # body 走临时文件而不是 argv：Windows 原生 curl.exe 会把含中文的 argv 按 ANSI 代码页（如 CP936）转码，
  # 服务端 serde_json 因非法 UTF-8 直接 400。@文件 原样传 UTF-8 字节；文件在 TempDir 内随脚本清理。
  $tokenBodyFile = Join-Path $script:TempDir 'token_create_body.json'
  [System.IO.File]::WriteAllText($tokenBodyFile, $tokenBody, [System.Text.UTF8Encoding]::new($false))
  $code = (& $curl -sS --connect-timeout 2 --max-time 5 -o $tokenFile -w '%{http_code}' -X POST -H 'Content-Type: application/json' -H "Authorization: Bearer $AliceToken" --data-binary "@$tokenBodyFile" "$base/admin/tokens" 2>$null | Out-String).Trim()
  if ($code -ne '201') {
    $sw.Stop()
    Fail-E2E 'e2e: admin token create + mcp tools/list filter' $sw.Elapsed.TotalSeconds '' ("POST /admin/tokens 期望 201，实际 {0}" -f $code)
  }
  $ok = Invoke-NodeCheck $tokenFile 'const d=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));const t=d.token||"";if(!t.startsWith("jumcp_")||t.length!==70){console.error("token 明文形状非法");process.exit(1)}const i=d.item||{};if(i.status!=="active"){console.error("item.status="+i.status);process.exit(1)}if((i.builtin_tools||[]).length!==2){console.error("builtin_tools 不符");process.exit(1)}if(i.token!==undefined||i.token_hash!==undefined){console.error("item 泄露明文/hash");process.exit(1)}'
  if (-not $ok) {
    $sw.Stop()
    Fail-E2E 'e2e: admin token create + mcp tools/list filter' $sw.Elapsed.TotalSeconds $script:CheckLog '创建响应契约不符'
  }
  $tokenJson = Get-Content -Raw -Path $tokenFile | ConvertFrom-Json
  $platformToken = $tokenJson.token
  $platformTokenId = $tokenJson.item.id
  $platformTools = Join-Path $script:TempDir 'platform_tools.json'
  & $curl -sS --connect-timeout 2 --max-time 5 -o $platformTools -X POST -H 'Content-Type: application/json' -H "Authorization: Bearer $platformToken" --data $listBody "$base/mcp" 2>$null | Out-Null
  $ok = Invoke-NodeCheck $platformTools 'const d=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));const names=((d.result||{}).tools||[]).map(t=>t.name);const need=["search_capabilities","describe_capability"];const miss=need.filter(n=>!names.includes(n));if(miss.length){console.error("缺少 allowlist 内工具: "+miss.join(","));process.exit(1)}const forbidden=names.filter(n=>["list_groups"].includes(n)||n.startsWith("admin_"));if(forbidden.length){console.error("出现未授权工具: "+forbidden.join(","));process.exit(1)}'
  if (-not $ok) {
    $sw.Stop()
    Fail-E2E 'e2e: admin token create + mcp tools/list filter' $sw.Elapsed.TotalSeconds $script:CheckLog 'allowlist 过滤失败'
  }
  $revokeFile = Join-Path $script:TempDir 'token_revoke.json'
  $code = (& $curl -sS --connect-timeout 2 --max-time 5 -o $revokeFile -w '%{http_code}' -X POST -H "Authorization: Bearer $AliceToken" "$base/admin/tokens/$platformTokenId/revoke" 2>$null | Out-String).Trim()
  if ($code -ne '200') {
    $sw.Stop()
    Fail-E2E 'e2e: admin token create + mcp tools/list filter' $sw.Elapsed.TotalSeconds '' ("POST /admin/tokens/{id}/revoke 期望 200，实际 {0}" -f $code)
  }
  $ok = Invoke-NodeCheck $revokeFile 'const d=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));if(d.status!=="revoked"||!d.revoked_at){console.error("撤销响应非 item/revoked");process.exit(1)}if(d.item!==undefined){console.error("撤销响应不得再包 item");process.exit(1)}'
  if (-not $ok) {
    $sw.Stop()
    Fail-E2E 'e2e: admin token create + mcp tools/list filter' $sw.Elapsed.TotalSeconds $script:CheckLog '撤销响应契约不符'
  }
  $afterRevoke = Join-Path $script:TempDir 'platform_tools_revoked.json'
  & $curl -sS --connect-timeout 2 --max-time 5 -o $afterRevoke -X POST -H 'Content-Type: application/json' -H "Authorization: Bearer $platformToken" --data $listBody "$base/mcp" 2>$null | Out-Null
  $ok = Invoke-NodeCheck $afterRevoke 'const d=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));const code=(((d.error||{}).data)||{}).mcp_code;if(code!=="MCP_UNAUTHORIZED"){console.error("撤销后应拒绝，实际 mcp_code="+code);process.exit(1)}'
  $sw.Stop()
  if (-not $ok) {
    Fail-E2E 'e2e: admin token create + mcp tools/list filter' $sw.Elapsed.TotalSeconds $script:CheckLog '撤销后仍可用'
  }
  Add-Step 'e2e: admin token create + mcp tools/list filter' 'PASS' $sw.Elapsed.TotalSeconds $serverLog

  Write-Summary
  Stop-Server
  Remove-Temp
  if ($script:FailCount -gt 0) { exit 1 }
  exit 0
} catch {
  Write-Host ("未预期错误：{0}" -f $_.Exception.Message)
  Add-Step '脚本执行' 'FAIL' 0 ''
  Write-Summary
  Stop-Server
  Remove-Temp
  exit 1
} finally {
  Stop-Server
}
