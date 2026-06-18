# Third-party data & attribution

## IP geolocation — DB-IP City Lite

The **Network discovery map** plots discovered libp2p/IPFS peers at their real
geographic location. Those locations come from the **DB-IP City Lite** database,
bundled offline inside the binary (`crates/gui/src/dbip-city-lite.mmdb.gz`) so the
lookup never calls an external service.

- Source: **DB-IP** — <https://db-ip.com>
- Database: *IP to City Lite*
- License: **Creative Commons Attribution 4.0 International (CC BY 4.0)** —
  <https://creativecommons.org/licenses/by/4.0/>

Per the license, this product includes IP geolocation data created by DB-IP.com,
available from <https://db-ip.com>. The required attribution is also shown in the
app's Network-map caption ("Geo © DB-IP (CC BY 4.0)").

The database is a point-in-time snapshot; locations are approximate and peers
without a resolvable public IP (relay-only / LAN) fall back to a stylized position.
