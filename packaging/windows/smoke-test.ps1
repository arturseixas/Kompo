# Installs target\Kompo.msi silently, starts the editor, checks that it came up, and uninstalls (run on the CI runner).
$ErrorActionPreference = "Stop"
$msi = (Resolve-Path "target\Kompo.msi").Path
$exe = "C:\Program Files\Kompo\kompo.exe"
$log = Join-Path $env:LOCALAPPDATA "Kompo\kompo.log"

function Fail($m) { Write-Host "::error::$m"; if (Test-Path $log) { Write-Host "---- kompo.log"; Get-Content $log }; exit 1 }

$p = Start-Process msiexec -ArgumentList "/i `"$msi`" /qn /l*v target\install.log" -Wait -PassThru
if ($p.ExitCode -ne 0) { Get-Content target\install.log -Tail 80; Fail "install failed with code $($p.ExitCode)" }
foreach ($f in @($exe, "C:\Program Files\Kompo\avcodec-*.dll",
    "C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Kompo.lnk", "C:\Users\Public\Desktop\Kompo.lnk")) {
  if (-not (Test-Path $f)) { Fail "missing after install: $f" }
}
Write-Host "installed; files and shortcuts are there"

# no dialogs (nobody to click them) and no sound device on the runner
$env:KOMPO_NO_DIALOG = "1"; $env:KOMPO_NULL_AUDIO = "1"
$app = Start-Process $exe -PassThru
Start-Sleep -Seconds 25   # longer than the supervisor's 10 s start-up window
$alive = @(Get-Process kompo -ErrorAction SilentlyContinue)
if (-not (Test-Path $log)) { Fail "the editor wrote no log file" }
Write-Host "---- kompo.log"; Get-Content $log
if (-not ((Get-Content $log -Raw) -match "application initialised")) { Fail "the editor did not finish starting" }
if ($alive.Count -lt 1) { Fail "the editor is not running 25 s after start" }
Write-Host "the editor is running ($($alive.Count) process(es))"
Stop-Process -Name kompo -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2

$p = Start-Process msiexec -ArgumentList "/x `"$msi`" /qn /l*v target\uninstall.log" -Wait -PassThru
if ($p.ExitCode -ne 0) { Get-Content target\uninstall.log -Tail 80; Fail "uninstall failed with code $($p.ExitCode)" }
if (Test-Path $exe) { Fail "kompo.exe is still there after uninstall" }
Write-Host "uninstalled cleanly"
