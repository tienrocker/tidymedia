# release.ps1 - bump version -> changelog -> commit -> tag -> push
# usage: .\scripts\release.ps1 [major|minor|patch|<x.y.z>]
param(
    [Parameter(Mandatory = $true)]
    [string]$Bump
)

$ErrorActionPreference = "Stop"
$repo = Split-Path $PSScriptRoot -Parent
Set-Location $repo

# git in warning (CRLF...) ra stderr; PowerShell 5.1 bien no thanh NativeCommandError
# va giet script voi EAP=Stop. Bao moi lenh git qua helper nay.
function Invoke-Git {
    $ErrorActionPreference = "Continue"
    & git @args 2>&1 | ForEach-Object { "$_" } | Write-Host
    if ($LASTEXITCODE -ne 0) { throw "git $($args -join ' ') failed ($LASTEXITCODE)" }
}

# Version sống duy nhất ở workspace Cargo.toml ([workspace.package]).
$cargoToml = Join-Path $repo "Cargo.toml"
$content = Get-Content $cargoToml -Raw
if ($content -notmatch 'version\s*=\s*"(\d+)\.(\d+)\.(\d+)"') {
    throw "Không tìm thấy version trong Cargo.toml"
}
$cur = [version]("{0}.{1}.{2}" -f $Matches[1], $Matches[2], $Matches[3])

switch -Regex ($Bump) {
    '^major$' { $new = "{0}.0.0" -f ($cur.Major + 1) }
    '^minor$' { $new = "{0}.{1}.0" -f $cur.Major, ($cur.Minor + 1) }
    '^patch$' { $new = "{0}.{1}.{2}" -f $cur.Major, $cur.Minor, ($cur.Build + 1) }
    '^\d+\.\d+\.\d+(-[0-9A-Za-z\.\-]+)?$' { $new = $Bump }
    default { throw "Bump không hợp lệ: $Bump (dùng major|minor|patch|x.y.z)" }
}

Write-Host "Version: $cur -> $new"

# 1) Bump Cargo.toml (chỉ dòng đầu tiên khớp — nằm trong [workspace.package])
$content = $content -replace "(?m)^version\s*=\s*`"$([regex]::Escape($cur.ToString()))`"", "version = `"$new`""
Set-Content $cargoToml $content -Encoding utf8 -NoNewline

# 2) Sync package.json (cosmetic)
$pkgPath = Join-Path $repo "package.json"
$pkg = Get-Content $pkgPath -Raw | ConvertFrom-Json
$pkg.version = $new
($pkg | ConvertTo-Json -Depth 10) + "`n" | Set-Content $pkgPath -Encoding utf8 -NoNewline

# 3) Cập nhật Cargo.lock cho version mới
cargo update --workspace --quiet

# 4) Changelog qua git-cliff neu co
if (Get-Command git-cliff -ErrorAction SilentlyContinue) {
    git-cliff --tag "v$new" -o CHANGELOG.md
    Invoke-Git add CHANGELOG.md
} else {
    Write-Host "git-cliff khong co trong PATH - bo qua CHANGELOG (cargo install git-cliff)"
}

# 5) Commit + tag + push
Invoke-Git add Cargo.toml Cargo.lock package.json
Invoke-Git -c core.safecrlf=false commit -m "chore(release): v$new"
Invoke-Git tag "v$new"
Write-Host "Da tag v$new. Push bang:"
Write-Host "  git push --follow-tags"
