# Cloudflared service installer endpoint

Tango Bridge exposes a local endpoint that can install the Cloudflare Tunnel service using a connector token. This lets you ship a `cloudflared.exe` alongside `tango_bridge.exe` and provision the tunnel automatically from the desktop app.

## Requirements

- Place `cloudflared.exe` in the same directory as `tango_bridge.exe` (the folder you run the bridge from).
- Run on Windows with sufficient privileges for `cloudflared service install` (typically Administrator).
- The bridge must be running locally (default: `http://localhost:15038`).

## Endpoint

- **URL:** `POST http://localhost:15038/cloudflared/install`
- **Body:** JSON

```json
{
  "token": "<connector_token_from_Cloudflare>"
}
```

### Example (PowerShell)

```powershell
Invoke-RestMethod \
  -Uri "http://localhost:15038/cloudflared/install" \
  -Method Post \
  -ContentType "application/json" \
  -Body (@{ token = "YOUR_TOKEN_HERE" } | ConvertTo-Json)
```

The endpoint will run:

```text
cloudflared.exe service install "<token>"
```

Output from `cloudflared` is returned in the HTTP response body. If `cloudflared.exe` cannot be found next to the bridge executable, the endpoint returns `400 Bad Request` explaining the missing file.
