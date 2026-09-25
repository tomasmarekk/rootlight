# Fetch the pinned Unicode case-fold data used by Rootlight's first-party mapper.
# This developer-only step is never invoked by Cargo or the release build.
$ErrorActionPreference = 'Stop'
$uri = 'https://www.unicode.org/Public/9.0.0/ucd/CaseFolding.txt'
$expectedHash = '37d40cf8c2c35637f4a04e746814e1fc4eb764c272bed9238a87ee96a4866857'
$target = Join-Path $PSScriptRoot 'CaseFolding-9.0.0.txt'
$client = [Net.Http.HttpClient]::new()
try {
    $bytes = $client.GetByteArrayAsync($uri).GetAwaiter().GetResult()
} finally {
    $client.Dispose()
}
$observedHash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes)).ToLowerInvariant()
if ($observedHash -cne $expectedHash) {
    throw 'Unicode case-fold data hash mismatch'
}
if (Test-Path -LiteralPath $target) {
    $existingHash = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($existingHash -cne $expectedHash) {
        throw 'Existing Unicode case-fold data differs from the pinned source'
    }
    return
}
[IO.File]::WriteAllBytes($target, $bytes)
