#!/usr/bin/env python3
"""Qualify the real screen boundary against an explicitly supplied core contract.

The library's public exports stay unchanged until coordinated core integration.
This runner compiles the exact src/screen.rs consumed by that integration and
owns a disposable X server. Its output directory must be outside this repository.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tomllib


def run(argv, **kwargs):
    subprocess.run(argv, check=True, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--domain", type=Path, required=True,
                        help="core domain crate providing the agreed screen module")
    parser.add_argument("--provider-root", type=Path, required=True)
    parser.add_argument("--legacy-provider", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[1]
    output = args.output.resolve()
    if output.is_relative_to(repo):
        raise ValueError("qualification artifacts must remain outside the repository")
    output.mkdir(mode=0o700, parents=True, exist_ok=True)
    domain = args.domain.resolve(strict=True)
    domain_package = tomllib.loads((domain / "Cargo.toml").read_text())["package"]
    package, version = domain_package["name"], domain_package["version"]
    run(["python3", str(repo / "scripts/prepare-screen-build.py"), str(args.provider_root)])
    build_spec = json.loads((repo / "providers/screen-build.lock.json").read_text())
    engine = args.provider_root.resolve() / "build/engine"
    legacy = json.loads((repo / "provider.lock.json").read_text())
    archive = args.legacy_provider / legacy["linux_asset"]
    if archive.stat().st_size != legacy["size"] or hashlib.sha256(archive.read_bytes()).hexdigest() != legacy["sha256"]:
        raise ValueError("legacy provider archive mismatch")
    run(["gh", "attestation", "verify", str(archive), "--repo", legacy["repository"],
         "--signer-workflow", legacy["repository"] + "/.github/workflows/release.yml",
         "--source-ref", "refs/tags/" + legacy["release"], "--source-digest", legacy["source_commit"],
         "--deny-self-hosted-runners", "--bundle", str(repo / legacy["attestation_bundle"])], timeout=60)
    relative = legacy["executables"]["NDS_TEST_RDS_AGENT"]
    binary = (args.legacy_provider / relative).resolve(strict=True)
    if not binary.is_relative_to(args.legacy_provider.resolve()):
        raise ValueError("unsafe legacy binary path")
    with tarfile.open(archive) as bundle:
        member = bundle.getmember(relative)
        if not member.isfile() or hashlib.sha256(bundle.extractfile(member).read()).digest() != hashlib.sha256(binary.read_bytes()).digest():
            raise ValueError("legacy executable differs from verified archive")
    # TOML accepts JSON's quoted strings; these strings are never shell commands.
    quote = lambda value: json.dumps(str(value))
    manifest = ['[package]', 'name="nds-remote-screen-qualification"', 'version="0.0.0"', 'edition="2024"',
                'license="AGPL-3.0-only"', '[dependencies]',
                f'nddev-device-sync-domain={{package={quote(package)},version={quote("=" + version)},path={quote(domain)}}}']
    for name, features in [("rds-client", []), ("rds-core", []), ("rds-desktop", ["x11"]),
                           ("rds-agent", ["desktop", "transport-noq"]), ("rds-net", ["transport-noq"])]:
        manifest.append(f'{name}={{version="=0.1.0",path={quote(engine / "crates" / name)},features={json.dumps(features)}}}')
    manifest += ['tokio={version="1",features=["full"]}', 'tokio-util={version="0.7",features=["rt"]}',
                 'ed25519-dalek="3"', 'rand="0.10"', 'tempfile="3"', 'x11rb="0.14"', '[patch.crates-io]']
    for name in ["iroh", "noq", "noq-proto"]:
        manifest.append(f'{name}={{path={quote(engine / "vendor" / name)}}}')
    manifest.append(f'netlink-packet-core={{path={quote(args.provider_root.resolve() / "build/netlink-packet-core")}}}')
    manifest += ['[lints.rust]', 'unsafe_code="forbid"']
    (output / "Cargo.toml").write_text("\n".join(manifest) + "\n")
    (output / "src").mkdir(exist_ok=True)
    (output / "src/lib.rs").write_text(
        f'#[path = {quote(repo / "src/screen.rs")}]\npub mod adapter;\n'
        f'#[cfg(test)]\n#[path = {quote(repo / "qualification/screen.rs")}]\nmod tests;\n')
    shutil.copyfile(repo / "qualification/screen.Cargo.lock", output / "Cargo.lock")
    env = dict(os.environ)
    env.setdefault("CARGO_TARGET_DIR", str(output / "target"))
    env["RUSTUP_TOOLCHAIN"] = build_spec["toolchain"]
    env.pop("WAYLAND_DISPLAY", None)
    env.pop("WAYLAND_SOCKET", None)
    env.update(XDG_SESSION_TYPE="x11", NDS_OWNED_XVFB="1", NDS_SCREEN_LEGACY_AGENT=str(binary))
    run(["cargo", "fmt", "--all", "--", "--check"], cwd=output, env=env)
    target = build_spec["target"]
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", target], cwd=output, env=env))
    (output / "compiled-graph.json").write_text(json.dumps(metadata) + "\n")
    lock = tomllib.loads((output / "Cargo.lock").read_text())
    macro = build_spec["macro"]
    resolved_macro = [p for p in lock["package"] if p["name"] == macro["name"] and p["version"] == macro["version"]]
    if len(resolved_macro) != 1 or resolved_macro[0]["checksum"] != macro["archive_sha256"]:
        raise ValueError("compiled macro dependency differs from verified artifact")
    run(["cargo-deny", "--locked", "--manifest-path", str(output / "Cargo.toml"), "--config", str(repo / "deny.toml"), "--target", target, "check"], cwd=output, env=env)
    run(["cargo", "audit", "--deny", "warnings", "--file", str(output / "Cargo.lock")], cwd=output, env=env)
    clippy = subprocess.run(["cargo", "clippy", "--locked", "--all-targets", "--target", target, "--message-format=json", "--", "-D", "warnings"], cwd=output, env=env, text=True, stdout=subprocess.PIPE)
    (output / "clippy.jsonl").write_text(clippy.stdout)
    artifacts = [json.loads(line) for line in clippy.stdout.splitlines() if line.startswith("{")]
    if clippy.returncode:
        for item in artifacts:
            if item.get("reason") == "compiler-message" and item["message"].get("rendered"):
                sys.stderr.write(item["message"]["rendered"])
        raise RuntimeError("screen qualification Clippy failed; diagnostics preserved in the private output")
    features = [{"package_id": item["package_id"], "features": item["features"], "kind": item["target"]["kind"]}
                for item in artifacts if item.get("reason") == "compiler-artifact"]
    (output / "compiled-features.json").write_text(json.dumps(features, indent=2) + "\n")
    run(["python3", str(repo / "scripts/owned-x11.py"), "--", "cargo", "test", "--locked", "--target", target, "--", "--test-threads=1"], cwd=output, env=env)
    run(["python3", str(repo / "scripts/owned-x11.py"), "--", "cargo", "nextest", "run", "--locked", "--target", target, "-j", "1"], cwd=output, env=env)
    wide_env = dict(env, NDS_ULTRAWIDE_FIXTURE="1")
    run(["python3", str(repo / "scripts/owned-x11.py"), "--screen", "5120x1440x24", "--", "cargo", "test", "--locked", "--target", target, "tests::a_real_ultrawide_source_fits_the_existing_frame_budget", "--", "--ignored", "--exact", "--test-threads=1"], cwd=output, env=wide_env)
    receipt = {"source": json.loads((repo / "providers/screen-source.lock.json").read_text()),
               "build": build_spec,
               "compiled_graph_sha256": hashlib.sha256((output / "compiled-graph.json").read_bytes()).hexdigest(),
               "compiled_features_sha256": hashlib.sha256((output / "compiled-features.json").read_bytes()).hexdigest(),
               "domain_package": package, "domain_manifest_sha256": hashlib.sha256((domain / "Cargo.toml").read_bytes()).hexdigest(),
               "domain_screen_sha256": hashlib.sha256((domain / "src/screen.rs").read_bytes()).hexdigest(),
               "adapter_sha256": hashlib.sha256((repo / "src/screen.rs").read_bytes()).hexdigest(),
               "acceptance_sha256": hashlib.sha256((repo / "qualification/screen.rs").read_bytes()).hexdigest(),
               "cargo_lock_sha256": hashlib.sha256((output / "Cargo.lock").read_bytes()).hexdigest(),
               "scope": "Linux X11 isolated capture/transport/decode; NDS application/presentation integration unqualified"}
    (output / "qualification.json").write_text(json.dumps(receipt, indent=2) + "\n")


if __name__ == "__main__":
    main()
