# ZiSK proving stack images

Three images, built from one [`Dockerfile`](Dockerfile) with `--target`, run
the ZiSK second proof system on a GPU machine in coordinator mode: the
proving keys and the GPU load once into a resident worker, and every proof
after that reuses them.

| Image | Target | Contents | Needs |
|---|---|---|---|
| `zksync-os-zisk-coordinator` | `coordinator` | `zisk-coordinator` from the pinned ZiSK tarball | CPU only |
| `zksync-os-zisk-worker` | `worker` | `zisk-worker` (GPU build), `cargo-zisk-dev`, the ASM setup toolchain, `zisk-prepare-keys` | an NVIDIA GPU, the key volume |
| `zksync-os-zisk-prover` | `prover` | `zksync-os-zisk-prover-service`, both hash-verified guest ELFs, the CPU `cargo-zisk` | the sequencer and the coordinator |

Every ZiSK binary comes out of the same release tarball, verified against the
sha256 pinned in the Dockerfile before extraction. Nothing from ZiSK is
compiled in these images, and none of them carries `snarkjs` or Node.js: the
coordinator path never verifies a wrapped proof locally, and the daemon's
`cargo-zisk remote` calls have no verify flag. The GPU worker links only the
driver's `libcuda.so.1`, which the NVIDIA container toolkit mounts at run
time, so the images sit on plain Ubuntu rather than a CUDA runtime image. The
tarball is x86_64 only, so the images are `linux/amd64`.

## Proving keys

The STARK key (3.8 GB compressed) and the PLONK key (21.9 GB compressed)
are not in any image. `zisk-prepare-keys`, shipped in the worker image and
run as a one-shot service before the worker, downloads both from Polygon's
`zisk-setup` bucket for the image's ZiSK version, checks the bucket's md5
sidecars and the sha256 pins in [`keys.sha256`](keys.sha256), extracts them
into the key volume, and runs the same constant-tree generation `ziskup`
performs. A marker in the volume makes later runs a no-op.

A pin that reads `PENDING` stops the run unless `ZISK_KEYS_ALLOW_UNPINNED=1`
is set, in which case the run trusts the md5 alone and prints the observed
sha256 so it can be recorded and reviewed. Plan for about 80 GB in the key
volume during installation and 40 GB after.

## Running the stack

```bash
cd docker/zisk-stack
cp .env.example .env         # ZISK_SEQUENCER_URL, image tag, GPU index
docker compose up -d         # first start fetches the keys
docker compose logs -f worker prover
```

[`compose.yaml`](compose.yaml) wires the pieces: `prepare-keys` completes,
the worker joins the coordinator on its cluster port, and the daemon
registers both guest ELFs through `cargo-zisk remote setup` and starts
polling the sequencer in aggregated mode. The daemon retries that setup until
a worker has finished loading its keys, which takes several minutes on a
cold start, and re-runs it once whenever a prove fails, so a coordinator
restart heals without restarting the daemon.

Ports on loopback: 7000 (coordinator client API), 9090 (coordinator metrics
and `/health`), 3313 (daemon metrics).

Host requirements: the NVIDIA container toolkit, a GPU with 16 GB or more of
VRAM, 64 GB or more of RAM (the PLONK key stays resident), and disk for the
key volume.

### Worker flags worth knowing

The compose file starts the worker with `--plonk --preload-plonk --gpu
--emulator`. Dropping `--emulator` selects the ASM emulator, which is faster
at witness generation; the image carries the build toolchain it assembles
with, and the compose file already lifts the memlock limit it needs. One
worker serves one GPU; copy the service with another `CUDA_VISIBLE_DEVICES`
for more, all joining the same coordinator.

## Building

```bash
# Developer build: daemon compiled in a container, ELFs from the reproducible builds
docker/zisk-stack/build-images.sh --prover-from-docker

# Release build: the released daemon binary and ELFs, SHA256SUMS-verified,
# so the image carries the exact bytes the release manifest pins
docker/zisk-stack/build-images.sh --prover-from-release 0.0.6 --registry ghcr.io/matter-labs --tag 0.0.6 --push
```

The coordinator and worker targets need nothing from this checkout beyond
the Dockerfile. The prover target copies `out/zksync-os-zisk-guest`,
`out/zksync-os-zisk-guest-aggregator` and `out/zksync-os-zisk-prover-service`
from the build context and re-verifies the ELFs against the recorded
`GUEST_ELF_SHA256` pins, so a stale `out/` cannot ship.

CI builds all three on pushes to `main` (`stage-build.yaml`). On a release
tag, `stage-build.yaml` builds the coordinator and worker images and
`release-assets.yaml` builds the prover image from the released assets;
release images also go to quay.

## Files

| File | Purpose |
|---|---|
| `Dockerfile`, `Dockerfile.dockerignore` | The three targets plus the `prover-export` helper |
| `coordinator.toml`, `coordinator-core.toml` | Coordinator service and core config (ports, JSON logs, no proof persistence) |
| `worker.toml` | Worker config; key paths and GPU flags stay on the command line |
| `prepare-keys.sh` | Installed as `zisk-prepare-keys` in the worker image |
| `keys.sha256` | sha256 pins of the key tarballs |
| `compose.yaml`, `.env.example` | Single-machine deployment |
| `build-images.sh` | Builds the images from a checkout or a release |
