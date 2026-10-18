# Build and release checks

`package_core.py` packages the core release executables.

The **Native adapter builds** workflow compiles the pinned Windows Mesen, Dolphin,
and PPSSPP recipes through manual dispatch. Select changed native inputs for a fresh
build; core version changes alone do not require these builds. Its exact cache key includes adapter
sources, patches, recipe, target and runner toolchain version. The three PowerShell
recipes do not consume MSYS2 shared helpers, so those helpers do not invalidate them.
Every retained runtime file and its signed producer lineage are verified before reuse. Missing or invalid cache
contents cause a fresh build.

The workflow retains runtime payloads, file-hash manifests and build logs for seven days.
These are compile checks; game runtime qualification is recorded separately.
Native binary releases and their corresponding source bundles are not published
by this workflow. Rust bridges continue to follow the core version.

Run the cache identity and payload checks locally:

```sh
python3 _tests/release/native_build_tests.py
python3 _tests/release/native_provenance_tests.py
python3 _tests/release/native_release_tests.py
python3 _tests/release/publish_release_tests.py
python3 _tests/release/native_unix_build_tests.py
```

`native_provenance.py` authenticates the producing repository, workflow, approved pipeline,
run/attempt and successful matrix job before accepting native payloads. Unix reuse, Windows
packaging, collection and release preparation retain signed parent records. Native source,
packaging and core revisions remain distinct; core-only changes permit reuse when native inputs
match. Unsigned historical packages require a fresh build. The native workflows attach a signed
`NATIVE-PROVENANCE.json` to each eligible payload.

`native_release.py` prepares the exact authenticated assembly and core asset list.
`publish_release.py` checks the complete draft asset inventory before publication, including
previous uploads. Matching partial uploads can resume. `NATIVE-PROVENANCE.tar.gz` preserves the
signed assembly and its parent proofs in the release. Signing and platform execution are verified
separately from the local admission fixtures.

The **Native MSYS2 adapter builds** workflow compiles Mednafen, Flycast, NP2kai,
Mupen64Plus, openMSX, MAME PC-98, MAME Neo Geo, PCSX2, and DeSmuME on Windows from their shell
recipes. MAME builds only the adapter's drivers. It records compiler/package versions,
build output, and the resulting executable or core DLL hash. Build results and game-runtime qualification are separate evidence. Successful builds retain runnable payloads with
native DLL dependencies and required assets for seven days; core-only changes do
not trigger them. Manual dispatch can select one adapter for a targeted retry.
PCSX2's shell recipe holds the work-tree lock while its PowerShell helper builds
the upstream dependencies and emulator with Visual Studio C++ tools.

The **Native xemu Windows build** workflow uses the upstream Windows cross-toolchain
container, pinned by digest. It applies the maintained source patches and records
the Windows executable hash and retains the Windows distribution for seven days;
Windows runtime validation remains separate.

`inspect_github_artifact.py` reads a retained ZIP's directory and native manifest
using bounded HTTP ranges. It requires an authenticated `gh` CLI and refuses a
bulk-download fallback. This checks package contents before remote deployment;
the full archive digest must still be verified where the archive is downloaded.

`windows_software_gl.ps1` installs a digest-pinned software OpenGL package beside
selected validation executables. Supply a private plan with `asset.url`,
`asset.digest` (`sha256:...`) and application `directories` relative to the
validation root. It writes a deployment record and leaves renderer behavior to
the runtime witnesses.


`deploy_windows_payloads.py` deploys already-downloaded qualification ZIPs from a
private plan. It verifies each archive digest, supports a nested core ZIP and
member prefix, and resumes only over identical destination files. Run it on the
validation host, then freeze the complete deployed layout with
`_tests/live/windows_runtime_inventory.py` before executing the batch.


`upload_qualification_inputs.py` uploads a reviewed private file manifest under an
operator-owned bucket's `inputs/` prefix. It verifies local bytes and reads back
object metadata, permits identical resumes and refuses conflicting existing
objects. Downloaded-byte verification remains the remote preflight's job.

`deliver_windows_artifacts.py` runs on the operator machine after the Windows
host is ready for SSM commands. It obtains a fresh Actions download URL for each
archive and sends a download-and-hash command to that host. The GitHub token and
archive bytes stay off the operator-to-host transfer path. Its local ledger
retains command IDs so interrupted observation resumes the same command; only a
terminal failed attempt permits a retry with a new URL. Supply a complete
`deploy_windows_payloads.py` plan with each Actions entry's `artifact_id`.

## Release and recovery

Use the public **Release binaries** manual workflow. Create the draft and immutable
product tag first, and select the completed native assembly as `native_run`. The
workflow checks draft visibility and tag identity before building. The preflight
job requires `contents: write` because GitHub hides drafts from read-only tokens.

For a failed publication with successful core builds, pass the original release
run as `core_run`. The workflow requires all three producing jobs from its latest
attempt to have succeeded, then checks downloaded package version, source, target
and digests. It skips compilation even when that run's publisher failed.

```sh
gh workflow run release-core.yml --repo OWNER/REPO --ref REVIEWED_PIPELINE_BRANCH \
  -f tag=VERSION -f native_run=ASSEMBLY_RUN -f core_run=CORE_RUN \
  -f validate_only=false
```

Keep the tag and selected producers fixed during recovery. A packaging correction
can run from a reviewed work branch; merge after successful publication. Native
producer admission continues to require its approved branches and signed lineage.

Preparation produces deterministic proof bytes. The publisher uploads missing
assets, accepts identical existing assets, rejects conflicts, and publishes only
after the full remote inventory matches. The `release-inventory` Actions artifact
retains the prepared checksums and native index even if publication fails.
Expired producer artifacts require restoring an admitted source of bytes or rebuilding
the affected producer; an absent artifact is not a reason to waive admission.
