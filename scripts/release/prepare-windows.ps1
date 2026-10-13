param([string]$BuildDirectory = "$PSScriptRoot/../../build/windows")
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'Run provisioning on Windows x64.' }
$BuildDirectory = [IO.Path]::GetFullPath($BuildDirectory)
$lock = Get-Content "$PSScriptRoot/windows-dependencies.json" -Raw | ConvertFrom-Json
$downloads = New-Item -ItemType Directory -Force "$BuildDirectory/downloads"

function Get-PinnedArtifact($Artifact, $Name) {
    $file = Join-Path $downloads $Name
    if (-not (Test-Path $file)) {
        Invoke-WebRequest $Artifact.url -OutFile "$file.partial"
        Move-Item "$file.partial" $file -Force
    }
    if ((Get-FileHash $file -Algorithm SHA256).Hash.ToLowerInvariant() -ne $Artifact.sha256) {
        Remove-Item $file
        throw "Checksum mismatch for $Name. No unverified files were extracted."
    }
    return $file
}

$vipsZip = Get-PinnedArtifact $lock.libvips 'libvips.zip'
$runtimeCab = Get-PinnedArtifact $lock.webview2 'webview2.cab'
foreach ($directory in @('native', 'payload', 'unpack')) {
    Remove-Item "$BuildDirectory/$directory" -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force "$BuildDirectory/$directory" | Out-Null
}
Expand-Archive $vipsZip "$BuildDirectory/unpack/vips"
$native = (Get-ChildItem "$BuildDirectory/unpack/vips" -Directory | Select-Object -First 1).FullName
Copy-Item "$native/*" "$BuildDirectory/native" -Recurse
# The vendor ships MSVC-compatible import libraries with lib-prefixed names.
foreach ($pair in @(@('libvips', 'vips'), @('libglib-2.0', 'glib-2.0'), @('libgobject-2.0', 'gobject-2.0'), @('libtiff', 'tiff'))) {
    Copy-Item "$native/lib/$($pair[0]).lib" "$BuildDirectory/native/lib/$($pair[1]).lib"
}
Copy-Item "$native/bin/*" "$BuildDirectory/payload" -Recurse
foreach ($directory in @('etc', 'share')) {
    if (Test-Path "$native/$directory") { Copy-Item "$native/$directory" "$BuildDirectory/payload" -Recurse }
}

New-Item -ItemType Directory -Force "$BuildDirectory/unpack/webview2" | Out-Null
& "$env:SystemRoot/System32/expand.exe" $runtimeCab '-F:*' "$BuildDirectory/unpack/webview2" | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Fixed WebView2 CAB extraction failed.' }
$runtimeExe = @(Get-ChildItem "$BuildDirectory/unpack/webview2" -Recurse -Filter msedgewebview2.exe)
if ($runtimeExe.Count -ne 1) { throw 'Expected exactly one Fixed WebView2 runtime.' }
$signature = Get-AuthenticodeSignature $runtimeExe[0].FullName
if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'Microsoft Corporation') {
    throw 'The pinned WebView2 binary must have a valid Microsoft signature.'
}
if ($runtimeExe[0].VersionInfo.ProductVersion -notlike "$($lock.webview2.version)*") {
    throw 'WebView2 version does not match the dependency lock.'
}
Copy-Item $runtimeExe[0].Directory.FullName "$BuildDirectory/payload/webview2" -Recurse

$licenses = New-Item -ItemType Directory -Force "$BuildDirectory/payload/licenses/libvips"
Copy-Item "$native/LICENSE", "$native/README.md", "$native/versions.json" $licenses
Copy-Item "$PSScriptRoot/../../LICENSE" "$BuildDirectory/payload/LICENSE.txt"
$licenseProvenance = Get-Content "$PSScriptRoot/windows-license-provenance.json" -Raw | ConvertFrom-Json
$sourceInventory = Get-Content "$PSScriptRoot/windows-lgpl-sources.json" -Raw | ConvertFrom-Json
$nativeVersions = Get-Content "$native/versions.json" -Raw | ConvertFrom-Json
foreach ($component in $sourceInventory.components) {
    if ($nativeVersions.PSObject.Properties[$component.nativeKey].Value -ne $component.version) {
        throw "Update the source inventory and license snapshot for $($component.component)."
    }
}
if ((Get-FileHash "$PSScriptRoot/windows-native-licenses.txt" -Algorithm SHA256).Hash.ToLowerInvariant() -ne $licenseProvenance.bundledTextSha256) {
    throw 'Native license snapshot does not match its provenance.'
}
Copy-Item "$PSScriptRoot/windows-native-licenses.txt", "$PSScriptRoot/windows-license-provenance.json" "$BuildDirectory/payload/licenses"
Copy-Item "$PSScriptRoot/windows-lgpl-sources.json" "$BuildDirectory/payload/licenses"
$notices = @"
Aperture Windows third-party notices

libvips $($lock.libvips.version) and its native dependencies are distributed as
unmodified shared libraries from the upstream Windows web build. The package
includes the vendor LGPL license, dependency versions, and library/source links
in licenses/libvips. Shared libraries may be replaced by compatible builds.
Native source and build recipes: https://github.com/libvips/build-win64-mxe/tree/v$($lock.libvips.version)
Exact LGPL component source versions and archives are listed in
licenses/windows-lgpl-sources.json. Corresponding-source distribution is a
first-release gate; notice collection alone does not complete that work.

Microsoft Edge WebView2 Fixed Runtime $($lock.webview2.version), Copyright Microsoft.
The complete runtime, including its embedded license and third-party credits,
is shipped under webview2. Its Fixed Runtime license terms also appear below.
It is serviced with Aperture releases.
https://developer.microsoft.com/microsoft-edge/webview2/

The native libraries use the Windows 10 Universal CRT supplied by Windows.
Aperture links the Rust MSVC runtime statically. No VC redistributable installer
or Evergreen WebView2 installer is run on the user's machine.
"@
$notices += "`n`nVendor license`n" + (Get-Content "$native/LICENSE" -Raw)
$notices += "`n`n" + (Get-Content "$PSScriptRoot/windows-native-licenses.txt" -Raw)
$notices | Set-Content "$BuildDirectory/native-notices.txt" -Encoding utf8
$info = [ordered]@{
    architecture = 'x86_64-pc-windows-msvc'
    libvips = $lock.libvips
    webview2 = $lock.webview2
    nativeComponents = $nativeVersions
    licenseProvenance = $licenseProvenance
    correspondingSources = $sourceInventory
    rustCrt = 'static'
}
$info | ConvertTo-Json -Depth 8 | Set-Content "$BuildDirectory/dependencies.json" -Encoding utf8
Write-Output "APERTURE_NATIVE_DIR=$BuildDirectory/native"
