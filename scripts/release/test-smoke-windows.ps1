# Test final-package byte comparison seams with files, without compiling or running packages.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$helper = Join-Path $PSScriptRoot 'smoke-windows.ps1'
$tokens = $null; $errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($helper, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw ($errors | Out-String) }
foreach ($name in @('Assert-Payload', 'Read-PayloadManifest')) {
    $definition = $ast.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name }, $true)
    if (!$definition) { throw "Missing package-byte helper $name" }
    Invoke-Expression $definition.Extent.Text
}
function Assert-Rejected($Action, $Message) {
    $rejected = $false
    try { & $Action } catch { $rejected = $_.ToString() -match $Message }
    if (!$rejected) { throw "Expected rejection: $Message" }
}
$temp = Join-Path ([IO.Path]::GetTempPath()) ('aperture-package-hashes-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force "$temp/webview2" | Out-Null
try {
    $expected = @{}
    foreach ($name in @('aperture.exe', 'libvips-42.dll', 'webview2/msedgewebview2.exe')) {
        [IO.File]::WriteAllText("$temp/$name", "validated $name")
        $expected[$name] = (Get-FileHash -LiteralPath "$temp/$name").Hash.ToLowerInvariant()
    }
    $lines = @($expected.Keys | Sort-Object | ForEach-Object { "$_ $($expected[$_])" })
    [IO.File]::WriteAllLines("$temp/PAYLOAD-SHA256.txt", $lines)
    $id = Read-PayloadManifest "$temp/PAYLOAD-SHA256.txt" $expected
    $knownId = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes(($lines -join "`n")))).ToLowerInvariant()
    if ($id -ne $knownId) { throw 'Portable extraction identity differs' }
    Assert-Payload $temp $expected @('PAYLOAD-SHA256.txt')
    [IO.File]::WriteAllText("$temp/.complete", $id)
    Assert-Payload $temp $expected @('PAYLOAD-SHA256.txt', '.complete')
    Remove-Item -LiteralPath "$temp/.complete" -Force
    [IO.File]::WriteAllText("$temp/aperture.exe", 'changed application')
    Assert-Rejected { Assert-Payload $temp $expected @('PAYLOAD-SHA256.txt') } 'payload differs: aperture.exe'
    [IO.File]::WriteAllText("$temp/aperture.exe", 'validated aperture.exe')
    [IO.File]::WriteAllText("$temp/libvips-42.dll", 'changed DLL')
    Assert-Rejected { Assert-Payload $temp $expected @('PAYLOAD-SHA256.txt') } 'payload differs: libvips-42.dll'
    [IO.File]::WriteAllText("$temp/libvips-42.dll", 'validated libvips-42.dll')
    Remove-Item -LiteralPath "$temp/aperture.exe"
    Assert-Rejected { Assert-Payload $temp $expected @('PAYLOAD-SHA256.txt') } 'payload differs: aperture.exe'
    [IO.File]::WriteAllText("$temp/aperture.exe", 'validated aperture.exe')
    [IO.File]::WriteAllText("$temp/unexpected.dll", 'unexpected executable code')
    Assert-Rejected { Assert-Payload $temp $expected @('PAYLOAD-SHA256.txt') } 'Unexpected packaged file'
    Remove-Item -LiteralPath "$temp/unexpected.dll"
    [IO.File]::WriteAllLines("$temp/PAYLOAD-SHA256.txt", $lines[0..1])
    Assert-Rejected { Read-PayloadManifest "$temp/PAYLOAD-SHA256.txt" $expected } 'omits validated payload'
    [IO.File]::WriteAllLines("$temp/PAYLOAD-SHA256.txt", @($lines + $lines[0]))
    Assert-Rejected { Read-PayloadManifest "$temp/PAYLOAD-SHA256.txt" $expected } 'manifest differs'
    [IO.File]::WriteAllText("$temp/PAYLOAD-SHA256.txt", "../outside.dll $('0' * 64)")
    Assert-Rejected { Read-PayloadManifest "$temp/PAYLOAD-SHA256.txt" $expected } 'manifest differs'
    Write-Output 'Final-package hash, missing/extra/changed file, inventory and portable extraction identity seams passed. No package was built or run.'
} finally {
    Remove-Item -LiteralPath $temp -Recurse -Force
}
exit 0
