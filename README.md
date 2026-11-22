# stubtr069

Stub TR-069 (CWMP) CPE daemon. It talks to a real ACS, accepts every RPC the ACS sends and
logs it, but **never changes the local system**: the data model lives in SQLite and
Download only fetches the file into a cache directory.

## Container image

Published to GitHub Container Registry by `.github/workflows/image.yml` on every push to `main`:

```sh
podman run -d --name stubtr069 --network host -v stubtr069-data:/data \
    ghcr.io/chlorodose/stubtr069:latest \
    --acs-url http://acs.example:9090/ --acs-username user --acs-password pass
```

State (SQLite DB and downloaded files) lives in `/data`. Use `--network host`, or publish `7547`
and set `--conn-req-url`/`STUB_CONN_REQ_URL` to the address the ACS can reach, otherwise the
advertised ConnectionRequestURL would be the container-internal address.

## Run from source

```sh
cargo run --release -- --acs-url http://acs.example:9090/ \
    --acs-username user --acs-password pass
```

Key options (all also available as `STUB_*` env vars, see `--help`): `--db`, `--download-dir`,
`--conn-req-listen` (default `0.0.0.0:7547`), `--conn-req-url` (advertised; auto-detected
otherwise), `--conn-req-username/--conn-req-password`, `--root Device|InternetGatewayDevice`.
Log level via `RUST_LOG` (`RUST_LOG=acs_command=debug` also prints the raw SOAP of each command).

## Behavior

| Area | Behavior |
| --- | --- |
| Sessions | Started on boot, by Connection Request, after a finished Download, after Reboot/FactoryReset, and by ScheduleInform. Failed sessions retry with exponential backoff. |
| Auth | HTTP Basic and Digest (MD5/SHA-256) towards the ACS and for incoming Connection Requests. |
| Data model | `Get/SetParameterValues`, `GetParameterNames`, `Get/SetParameterAttributes`, `AddObject`, `DeleteObject` operate on SQLite (`parameters`, `objects`, `object_tables`). Sets are atomic and type-checked; `ParameterKey` is tracked. Passwords always read back empty. |
| Download | Fetched over http into `--download-dir`, size checked, SHA-256 recorded in table `downloads`, then reported via `TransferComplete` (`7 TRANSFER COMPLETE` + `M Download`). Never applied. |
| Reboot / FactoryReset | Simulated: only the events and (for FactoryReset) the default data model are restored. |
| Command log | Every ACS RPC is logged (target `acs_command`, secrets redacted) and stored with its raw XML in table `commands`. |
| Unsupported | Logged with target `unsupported` and answered with a proper fault where needed: HTTPS (ACS or download URL, fault 9013), `Upload` and other RPCs (9000), enabling periodic inform and value-change notifications (value stored, no behavior). |

## Test

```sh
cargo test                               # unit tests
python3 scripts/mock_acs.py --connreq http://127.0.0.1:7547/ &   # mock ACS on :9090
cargo run -- --acs-url http://127.0.0.1:9090/ --acs-username acs --acs-password acspass \
    --conn-req-url http://127.0.0.1:7547/
```
