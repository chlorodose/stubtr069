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
otherwise), `--conn-req-username/--conn-req-password`, `--root Device|InternetGatewayDevice`, `--strict-model` (`STUB_STRICT_MODEL`), `--conn-req-url-script` / `--conn-req-url-interval` (see below).
Log level via `RUST_LOG` (`RUST_LOG=acs_command=debug` also prints the raw SOAP of each command).

## Behavior

| Area | Behavior |
| --- | --- |
| Sessions | Started on boot, by Connection Request, after a finished Download, after Reboot/FactoryReset, and by ScheduleInform. Failed sessions retry with exponential backoff. |
| Auth | HTTP Basic and Digest (MD5/SHA-256) towards the ACS and for incoming Connection Requests. |
| Data model | `Get/SetParameterValues`, `GetParameterNames`, `Get/SetParameterAttributes`, `AddObject`, `DeleteObject` operate on SQLite (`parameters`, `objects`, `object_tables`). By default the model is **not enforced** (enable validation with `--strict-model`, which rejects unknown parameters with 9005, read-only writes with 9008, bad values with 9007 and invalid object operations): setting an unknown parameter creates it (typed by the ACS-sent `xsi:type`, default `xsd:string`), reading an unknown parameter creates it with an empty value and returns it, and read-only flags, value types and object existence are never rejected. Known booleans/ints are still normalized when the value parses. `ParameterKey` is tracked; passwords always read back empty. |
| Download | Fetched over http into `--download-dir`, size checked, SHA-256 recorded in table `downloads`, then reported via `TransferComplete` (`7 TRANSFER COMPLETE` + `M Download`). Never applied. |
| Reboot / FactoryReset | Simulated: only the events and (for FactoryReset) the default data model are restored. |
| Command log | Every ACS RPC is logged (target `acs_command`, secrets redacted) and stored with its raw XML in table `commands`. |
| Unsupported | Logged with target `unsupported` and answered with a proper fault where needed: HTTPS (ACS or download URL, fault 9013), `Upload` and other RPCs (9000), enabling periodic inform and value-change notifications (value stored, no behavior). |

## Dynamic ConnectionRequestURL (DHCP)

If the host address can change, let a script report it: `--conn-req-url-script /path/to/script`
(`STUB_CONN_REQ_URL_SCRIPT`, exclusive with `--conn-req-url`). The script is executed directly
(no shell, 10 s timeout) and must print the URL, e.g. `http://192.0.2.7:7547/`, as the first
non-empty line of stdout. It gets `STUB_ACS_URL`, `STUB_CONN_REQ_LISTEN` and `STUB_CONN_REQ_PORT`
in its environment.

It runs at startup, before every session and every `--conn-req-url-interval` seconds
(default 30, `0` = only at startup/before sessions). When the URL changes, the
`ManagementServer.ConnectionRequestURL` parameter is updated and a session with the
`4 VALUE CHANGE` event is opened right away, so the ACS learns the new address. If the script fails
or prints something invalid, the previous URL is kept and a warning is logged. Without a script the
address is auto-detected once at startup. In the container, mount the script and make sure the tools
it uses exist in the image (it ships no `iproute2`).

```sh
#!/bin/sh
echo "http://$(ip -4 -o addr show dev eth0 | awk '{print $4}' | cut -d/ -f1):7547/"
```

## Test

```sh
cargo test                               # unit tests
python3 scripts/mock_acs.py --connreq http://127.0.0.1:7547/ &   # mock ACS on :9090
cargo run -- --acs-url http://127.0.0.1:9090/ --acs-username acs --acs-password acspass \
    --conn-req-url http://127.0.0.1:7547/
```
