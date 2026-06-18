//! Real geo-IP for the Network discovery map.
//!
//! Each discovered peer's multiaddr carries its IP; we resolve it to a true
//! latitude/longitude with a **bundled, offline** database — no per-peer call to
//! any external geo service, so mapping the swarm leaks nothing to a third party.
//!
//! The database is **DB-IP City Lite** (<https://db-ip.com>), licensed
//! **CC-BY-4.0**. It is gzip-embedded in the binary and decompressed once, lazily,
//! the first time a lookup happens. The required attribution is surfaced in the
//! map caption (see `index.html`).

use std::net::IpAddr;
use std::sync::OnceLock;

use maxminddb::{geoip2, Reader};

/// The bundled DB-IP City Lite database, gzip-compressed (~59 MB vs ~150 MB raw).
const DBIP_CITY_GZ: &[u8] = include_bytes!("dbip-city-lite.mmdb.gz");

/// Lazily decompressed + parsed reader. `None` if decompression/parse ever fails
/// (the map then just falls back to stylised positions — never an error).
static READER: OnceLock<Option<Reader<Vec<u8>>>> = OnceLock::new();

fn reader() -> Option<&'static Reader<Vec<u8>>> {
    READER
        .get_or_init(|| {
            let mut raw = Vec::new();
            let mut dec = flate2::read::GzDecoder::new(DBIP_CITY_GZ);
            std::io::Read::read_to_end(&mut dec, &mut raw).ok()?;
            Reader::from_source(raw).ok()
        })
        .as_ref()
}

/// True `(lat, lon, country_iso)` for an IP, or `None` when the DB has no fix.
pub fn locate(ip: IpAddr) -> Option<(f64, f64, Option<String>)> {
    let found = reader()?.lookup(ip).ok()?;
    let city: geoip2::City = found.decode().ok()??;
    let (lat, lon) = (city.location.latitude?, city.location.longitude?);
    let country = city.country.iso_code.map(|iso| iso.to_string());
    Some((lat, lon, country))
}

/// The first **public, routable** IP among a peer's multiaddrs (skips loopback,
/// private LAN, link-local, and unspecified addresses — those have no geo).
pub fn public_ip_from_addrs(addrs: &[String]) -> Option<IpAddr> {
    addrs.iter().find_map(|addr| {
        let mut parts = addr.split('/').filter(|s| !s.is_empty());
        while let Some(proto) = parts.next() {
            if proto == "ip4" || proto == "ip6" {
                if let Some(ip) = parts.next().and_then(|s| s.parse::<IpAddr>().ok()) {
                    if is_public(&ip) {
                        return Some(ip);
                    }
                }
            }
        }
        None
    })
}

/// Whether an address is a public, geo-locatable IP (not loopback/private/etc.).
fn is_public(ip: &IpAddr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    match ip {
        IpAddr::V4(v4) => !(v4.is_private() || v4.is_link_local() || v4.is_broadcast()),
        // No stable `is_unique_local`/`is_unicast_link_local` on stable Rust; fc00::/7
        // (ULA) and fe80::/10 (link-local) are rare on the public DHT, so a cheap
        // prefix check keeps them off the map without pulling in a crate.
        IpAddr::V6(v6) => {
            let seg = v6.segments()[0];
            (seg & 0xfe00) != 0xfc00 && (seg & 0xffc0) != 0xfe80
        }
    }
}

/// Convenience: resolve a peer's address list straight to coordinates.
pub fn locate_addrs(addrs: &[String]) -> Option<(f64, f64, Option<String>)> {
    locate(public_ip_from_addrs(addrs)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_first_public_ip_and_skips_local() {
        let addrs = vec![
            "/ip4/127.0.0.1/tcp/4001/p2p/Qm".to_string(),
            "/ip4/192.168.1.9/tcp/4001/p2p/Qm".to_string(),
            "/ip4/147.135.44.132/tcp/443/wss/p2p/Qm".to_string(),
        ];
        assert_eq!(
            public_ip_from_addrs(&addrs),
            Some("147.135.44.132".parse().unwrap())
        );
        // Only local/loopback ⇒ no geo.
        assert_eq!(
            public_ip_from_addrs(&["/ip4/10.0.0.5/tcp/4001".to_string()]),
            None
        );
    }

    #[test]
    fn bundled_db_resolves_a_known_public_ip() {
        // 8.8.8.8 (Google DNS) is in every city DB; assert we get a plausible fix.
        if let Some((lat, lon, _)) = locate("8.8.8.8".parse().unwrap()) {
            assert!((-90.0..=90.0).contains(&lat));
            assert!((-180.0..=180.0).contains(&lon));
        } else {
            panic!("bundled DB-IP database failed to resolve 8.8.8.8");
        }
    }
}
