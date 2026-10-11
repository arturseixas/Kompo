# Builds Kompo.msi on Windows (WiX v4: dotnet tool install --global wix). FFMPEG_DIR = LGPL shared build.
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")
$meta = cargo metadata --no-deps --format-version 1 | ConvertFrom-Json
$version = ($meta.packages | Where-Object name -eq "kompo-app").version
cargo build --release -p kompo-app
$dist = "target\dist"
Remove-Item $dist -Recurse -Force -ErrorAction SilentlyContinue
New-Item $dist -ItemType Directory | Out-Null
Copy-Item target\release\kompo.exe $dist
Copy-Item "$env:FFMPEG_DIR\bin\*.dll" $dist
# harvest: one component per file
$items = Get-ChildItem $dist -File | ForEach-Object {
  $id = "f" + ($_.BaseName -replace "[^A-Za-z0-9_]", "_")
  "      <Component Id=`"c_$id`"><File Id=`"$id`" Source=`"$($_.FullName)`" /></Component>"
}
$wxs = (Get-Content packaging\windows\kompo.wxs -Raw).Replace("<!-- target\dist\*.exe and *.dll are harvested by build-msi.ps1 -->", ($items -join "`n"))
Set-Content target\kompo.generated.wxs $wxs
# the wizard UI is an extension; its version must match the wix tool
$wixVersion = (wix --version).Split("+")[0]
wix extension add -g "WixToolset.UI.wixext/$wixVersion"
if ($LASTEXITCODE -ne 0) { throw "could not add the WiX UI extension" }
wix build target\kompo.generated.wxs -ext WixToolset.UI.wixext -d Version=$version -d PackagingDir="$PWD\packaging\windows" -arch x64 -o target\Kompo.msi
if ($LASTEXITCODE -ne 0) { throw "wix build failed" }
Write-Host "target\Kompo.msi"
