# Tai ffmpeg.exe + ffprobe.exe + LICENSE vao src-tauri\binaries\ neu chua du.
# Duoc goi tu beforeBuildCommand va release.yml — idempotent.
# PIN version + verify SHA256: release build phai reproducible, khong an theo
# "latest" cua gyan.dev (supply-chain). Bump version thi cap nhat ca 2 dong duoi.
$ErrorActionPreference = "Stop"

$FfmpegVersion = "8.1.2"
$FfmpegSha256 = "db580001caa24ac104c8cb856cd113a87b0a443f7bdf47d8c12b1d740584a2ec"

$dir = Join-Path $PSScriptRoot "..\src-tauri\binaries"
$ffmpeg = Join-Path $dir "ffmpeg.exe"
$ffprobe = Join-Path $dir "ffprobe.exe"
$license = Join-Path $dir "ffmpeg-LICENSE.txt"

if ((Test-Path $ffmpeg) -and (Test-Path $ffprobe) -and (Test-Path $license)) {
    Write-Host "ffmpeg/ffprobe/license da co trong src-tauri\binaries - bo qua download"
    exit 0
}

New-Item -ItemType Directory -Force $dir | Out-Null
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$url = "https://www.gyan.dev/ffmpeg/builds/packages/ffmpeg-$FfmpegVersion-essentials_build.zip"
$zip = Join-Path $env:TEMP "ffmpeg-$FfmpegVersion-essentials_build.zip"
Write-Host "Dang tai ffmpeg $FfmpegVersion essentials (gyan.dev, ~90MB)..."
Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing

$actual = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLowerInvariant()
if ($actual -ne $FfmpegSha256) {
    Remove-Item $zip -Force
    throw "SHA256 khong khop! expected=$FfmpegSha256 actual=$actual - zip bi thay doi?"
}

$extract = Join-Path $env:TEMP "ffmpeg-essentials-extract"
if (Test-Path $extract) { Remove-Item -Recurse -Force $extract }
Expand-Archive -Path $zip -DestinationPath $extract

$bin = Get-ChildItem $extract -Recurse -Filter ffmpeg.exe | Select-Object -First 1
if (-not $bin) { throw "khong tim thay ffmpeg.exe trong zip" }
Copy-Item $bin.FullName $ffmpeg -Force
Copy-Item (Join-Path $bin.Directory.FullName "ffprobe.exe") $ffprobe -Force
# GPLv3 build — LICENSE phai di kem binary trong moi ban phan phoi
$lic = Get-ChildItem $extract -Recurse -Filter "LICENSE*" | Select-Object -First 1
if ($lic) { Copy-Item $lic.FullName $license -Force }
else {
    Set-Content -Path $license -Encoding ascii -Value @"
Bundled ffmpeg/ffprobe are GPLv3 builds from https://www.gyan.dev/ffmpeg/builds/
FFmpeg source: https://ffmpeg.org - License: https://www.gnu.org/licenses/gpl-3.0.html
"@
}

Remove-Item $zip -Force
Remove-Item -Recurse -Force $extract
Write-Host "OK: $ffmpeg (v$FfmpegVersion, sha256 verified)"
