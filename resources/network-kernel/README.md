# Bundled network kernel

Fusion distributes the unmodified MetaCubeX/mihomo `v1.19.33` Windows amd64-compatible executable as a separate, locally managed process. Fusion does not enable its system-proxy, TUN, or DNS interception features.

- Upstream release: https://github.com/MetaCubeX/mihomo/releases/tag/v1.19.33
- Exact tagged source and build files: https://github.com/MetaCubeX/mihomo/tree/v1.19.33
- Source archive: https://github.com/MetaCubeX/mihomo/archive/refs/tags/v1.19.33.zip
- License: GNU GPL version 3; see the accompanying `LICENSE`.
- Archive and executable SHA256 values: `manifest.json`.

Run `powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/prepare-network-kernel.ps1` from the repository root to obtain and verify the exact artifact. Production packaging uses `-CheckOnly` and does not silently download a newer core. The application also checks the compiled-in executable digest before starting it.

All native verification fixtures in `scripts/verify-network-kernel.py` use temporary directories and loopback fake proxies; they are not evidence of real subscription or Codex acceptance.
