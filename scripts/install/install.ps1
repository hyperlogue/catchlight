# Install catchlight-editor from the GitHub releases:
#
#   irm https://github.com/hyperlogue/catchlight/releases/latest/download/install.ps1 | iex
#
# CATCHLIGHT_VERSION      a release tag such as v0.2.1 (default: the latest)
# CATCHLIGHT_INSTALL_DIR  where the binary goes (default: %LOCALAPPDATA%\catchlight\bin)
# CATCHLIGHT_DOWNLOAD_URL a mirror holding the release assets (overrides both)
#
# The archive is checked against the release's SHA256SUMS before anything is
# installed. The install directory is added to the user's PATH if missing,
# effective in this session and in terminals opened after it.
$ErrorActionPreference = 'Stop'

$repo = 'hyperlogue/catchlight'
# 32-bit PowerShell on 64-bit Windows reports x86 and keeps the truth here.
$arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
if ($arch -ne 'AMD64') {
    throw "no prebuilt catchlight-editor for $arch; try: cargo install catchlight-editor"
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
# Read and write the raw registry value so entries like %USERPROFILE%\bin stay
# unexpanded, as the Environment API would not leave them.
$key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
$userPath = $key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
if (-not (($userPath -split ';') -contains $dir)) {
    $joined = if ($userPath) { "$userPath;$dir" } else { $dir }
    $key.SetValue('Path', $joined, [Microsoft.Win32.RegistryValueKind]::ExpandString)
    # A raw registry write tells no one; broadcast WM_SETTINGCHANGE so
    # Explorer, and every terminal started from it, sees the new PATH without
    # signing out.
    if (-not ('CatchlightInstall.Environment' -as [type])) {
        Add-Type -Namespace CatchlightInstall -Name Environment -MemberDefinition @'
[DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern IntPtr SendMessageTimeout(
    IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam,
    uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);
'@
    }
    $result = [UIntPtr]::Zero
    # HWND_BROADCAST, WM_SETTINGCHANGE, SMTO_ABORTIFHUNG, 5 s.
    [void][CatchlightInstall.Environment]::SendMessageTimeout(
        [IntPtr]0xffff, 0x1A, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$result)
    # This session too, so the command works right away.
    $env:Path = "$env:Path;$dir"
    Write-Host "added $dir to your user PATH"
}
$key.Close()
