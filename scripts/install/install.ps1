# Install catchlight-editor from the GitHub releases:
#
#   irm https://github.com/hyperlogue/catchlight/releases/latest/download/install.ps1 | iex
#
# CATCHLIGHT_VERSION      a release tag such as v0.2.0 (default: the latest)
# CATCHLIGHT_INSTALL_DIR  where the binary goes (default: %LOCALAPPDATA%\catchlight\bin)
# CATCHLIGHT_DOWNLOAD_URL a mirror holding the release assets (overrides both)
#
# The archive is checked against the release's SHA256SUMS before anything is
# installed. The install directory is added to the user's PATH if missing.
$ErrorActionPreference = 'Stop'

$repo = 'hyperlogue/catchlight'
if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
    throw "no prebuilt catchlight-editor for $($env:PROCESSOR_ARCHITECTURE); try: cargo install catchlight-editor"
}
$target = 'x86_64-pc-windows-msvc'
$version = if ($env:CATCHLIGHT_VERSION) { $env:CATCHLIGHT_VERSION } else { 'latest' }
$base = if ($env:CATCHLIGHT_DOWNLOAD_URL) { $env:CATCHLIGHT_DOWNLOAD_URL }
        elseif ($version -eq 'latest') { "https://github.com/$repo/releases/latest/download" }
        else { "https://github.com/$repo/releases/download/$version" }
$dir = if ($env:CATCHLIGHT_INSTALL_DIR) { $env:CATCHLIGHT_INSTALL_DIR }
       else { Join-Path $env:LOCALAPPDATA 'catchlight\bin' }
$asset = "catchlight-editor-$target.zip"

$tmp = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $archive = Join-Path $tmp $asset
    $sums = Join-Path $tmp 'SHA256SUMS'
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile $archive
    Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS" -OutFile $sums
    $expected = Get-Content $sums |
        ForEach-Object { $parts = $_ -split '\s+'; if ($parts[1] -eq $asset -or $parts[1] -eq "*$asset") { $parts[0] } } |
        Select-Object -First 1
    $actual = (Get-FileHash -Algorithm SHA256 -Path $archive).Hash.ToLowerInvariant()
    if (-not $expected -or $expected -ne $actual) { throw "checksum mismatch for $asset; nothing was installed" }

    Expand-Archive -Path $archive -DestinationPath $tmp -Force
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    Copy-Item -Force (Join-Path $tmp "catchlight-editor-$target\catchlight-editor.exe") $dir
} finally {
    Remove-Item -Recurse -Force $tmp
}

$exe = Join-Path $dir 'catchlight-editor.exe'
Write-Host "installed $(& $exe --version) to $dir"
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not (($userPath -split ';') -contains $dir)) {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$dir", 'User')
    Write-Host "added $dir to your user PATH; open a new terminal to run catchlight-editor"
}
