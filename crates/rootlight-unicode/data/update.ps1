# Fetch the pinned Unicode data used by Rootlight's first-party mappers.
# This developer-only step is never invoked by Cargo or the release build.
$ErrorActionPreference = 'Stop'
$sources = @(
    @{
        Uri = 'https://www.unicode.org/Public/9.0.0/ucd/CaseFolding.txt'
        Name = 'CaseFolding-9.0.0.txt'
        Sha256 = '37d40cf8c2c35637f4a04e746814e1fc4eb764c272bed9238a87ee96a4866857'
    },
    @{
        Uri = 'https://www.unicode.org/Public/17.0.0/ucd/UnicodeData.txt'
        Name = 'UnicodeData-17.0.0.txt'
        Sha256 = '2e1efc1dcb59c575eedf5ccae60f95229f706ee6d031835247d843c11d96470c'
    },
    @{
        Uri = 'https://www.unicode.org/Public/17.0.0/ucd/CompositionExclusions.txt'
        Name = 'CompositionExclusions-17.0.0.txt'
        Sha256 = '2f239196ef3b5b61db5cc476e9bd80f534d15aa1b74e1be1dea5d042a344c85f'
    },
    @{
        Uri = 'https://www.unicode.org/Public/17.0.0/ucd/NormalizationTest.txt'
        Name = 'NormalizationTest-17.0.0.txt'
        Sha256 = '5019ffd530751a741900c849c0e010332f142a3612234639bd200b82138a87db'
    }
)
$client = [Net.Http.HttpClient]::new()
try {
    foreach ($source in $sources) {
        $target = Join-Path $PSScriptRoot $source.Name
        if (Test-Path -LiteralPath $target) {
            $existingHash = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($existingHash -cne $source.Sha256) {
                throw "Existing Unicode data differs from the pinned source: $($source.Name)"
            }
            continue
        }
        $bytes = $client.GetByteArrayAsync($source.Uri).GetAwaiter().GetResult()
        $observedHash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes)).ToLowerInvariant()
        if ($observedHash -cne $source.Sha256) {
            throw "Unicode data hash mismatch: $($source.Name)"
        }
        [IO.File]::WriteAllBytes($target, $bytes)
    }
} finally {
    $client.Dispose()
}
