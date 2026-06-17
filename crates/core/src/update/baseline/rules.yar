/*
 * UCP baked baseline ruleset — the last-known-good floor (Decision D-AU-4).
 *
 * Because the rules channel is IPNS-only, a fresh install with no node yet would
 * otherwise have ZERO rules. This file is `include_bytes!`'d into the binary at
 * build time so the YARA-X scanner is functional from byte one, fully offline.
 * IPNS updates only ever *supersede* this baseline; it also serves as the initial
 * rollback target before the first successful IPNS fetch.
 *
 * These are intentionally a tiny, conservative, low-false-positive seed — the rich
 * detection set arrives over the rules channel from the YARA Forge mirror. Keep this
 * compiling against the `yara-x` version the app currently ships.
 */

rule UCP_EICAR_Test_File
{
    meta:
        description = "Standard EICAR anti-malware test string (not a real threat)"
        severity = "test"
    strings:
        $eicar = "X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*"
    condition:
        $eicar
}

rule UCP_Eval_Over_Base64
{
    meta:
        description = "Obfuscated-payload pattern: dynamic eval over base64-decoded data"
        severity = "low"
    strings:
        $eval = "eval(" ascii nocase
        $b64a = "base64_decode" ascii nocase
        $b64b = "atob(" ascii nocase
    condition:
        $eval and any of ($b64*)
}

rule UCP_PHP_Webshell
{
    meta:
        description = "PHP file passing request input straight to a shell sink"
        severity = "medium"
    strings:
        $php = "<?php"
        $s1 = "system($_" ascii nocase
        $s2 = "shell_exec($_" ascii nocase
        $s3 = "passthru($_" ascii nocase
        $s4 = "proc_open($_" ascii nocase
    condition:
        $php and any of ($s*)
}
