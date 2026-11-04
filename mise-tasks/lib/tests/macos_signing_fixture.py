"""Create disposable signing credentials for native release and updater tests."""

import base64
import hashlib
import subprocess

TEAM = "ABCDE12345"
# Release signing selects the certificate by this name. Being self-signed, it has no Team ID.
NAME = f"Developer ID Application: SpaceTerm Test ({TEAM})"


def certificate(directory, *, private=True):
    directory.mkdir(parents=True, exist_ok=True)

    def run(*arguments):
        return subprocess.run(arguments, check=True, capture_output=True).stdout

    config = directory / "certificate.cnf"
    config.write_text(
        "[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n"
        f"[dn]\nCN={NAME}\n[ext]\n"
        "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\n"
        "extendedKeyUsage=critical,codeSigning\n"
    )
    key = directory / "private.pem"
    cert = directory / "certificate.pem"
    run(
        "openssl",
        "req",
        "-new",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-days",
        "1",
        "-config",
        str(config),
        "-keyout",
        str(key),
        "-out",
        str(cert),
    )
    key.chmod(0o600)
    der = run("openssl", "x509", "-in", str(cert), "-outform", "DER")
    fingerprint = hashlib.sha1(der).hexdigest().upper()
    exported = directory / "certificate.p12"
    password = "ephemeral-test-export"
    run(
        "openssl",
        "pkcs12",
        "-export",
        "-legacy",
        "-in",
        str(cert),
        *(["-inkey", str(key)] if private else ["-nokeys"]),
        "-out",
        str(exported),
        "-passout",
        f"pass:{password}",
    )
    return fingerprint, {
        "MACOS_SIGNING_CERTIFICATE_P12": base64.b64encode(exported.read_bytes()).decode(),
        "MACOS_SIGNING_CERTIFICATE_PASSWORD": password,
    }
