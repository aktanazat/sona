use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

pub(crate) fn signature(secret: &[u8], timestamp: &str, body: &[u8]) -> String {
    // SAFETY: HMAC-SHA256 accepts every key length; hmac 0.12's KeyInit always returns Ok.
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

pub(crate) fn public_url(value: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(value).map_err(|_| "Enter a complete HTTPS address.".to_owned())?;
    let host = url.host_str().ok_or("Enter a complete HTTPS address.")?;
    if value.len() > 4096 || url.scheme() != "https" || !url.username().is_empty()
        || url.password().is_some() || url.fragment().is_some()
        || crate::net_policy::is_private_relay_host(Some(host))
        || host.ends_with(".local") || host.ends_with(".localhost")
    {
        return Err("Use a public HTTPS address without a username, password or fragment.".into());
    }
    if let Some(url::Host::Ipv4(ip)) = url.host() {
        if !public_ip(IpAddr::V4(ip)) { return Err("Private network addresses are not allowed here.".into()); }
    }
    if let Some(url::Host::Ipv6(ip)) = url.host() {
        if !public_ip(IpAddr::V6(ip)) { return Err("Private network addresses are not allowed here.".into()); }
    }
    Ok(url)
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0 || a == 10 || a == 127 || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254) || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 168 || b == 0 || (b == 88 && c == 99)))
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            // Global unicast only. Exclude transition and documentation ranges.
            (s[0] & 0xe000) == 0x2000 && s[0] != 0x2002
                && !(s[0] == 0x2001 && (s[1] < 0x0200 || s[1] == 0x0db8))
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

async fn public_client(url: &url::Url) -> Result<reqwest::Client, String> {
    let host = url.host_str().ok_or("The destination has no host.")?;
    let addresses: Vec<SocketAddr> = match url.host() {
        Some(url::Host::Ipv4(ip)) => vec![SocketAddr::new(IpAddr::V4(ip), url.port_or_known_default().unwrap_or(443))],
        Some(url::Host::Ipv6(ip)) => vec![SocketAddr::new(IpAddr::V6(ip), url.port_or_known_default().unwrap_or(443))],
        _ => tokio::time::timeout(Duration::from_secs(10), tokio::net::lookup_host((host, url.port_or_known_default().unwrap_or(443))))
            .await.map_err(|_| "The destination lookup timed out.")?
            .map_err(|_| "The destination could not be found.")?.collect(),
    };
    if addresses.is_empty() || addresses.iter().any(|address| !public_ip(address.ip())) {
        return Err("This destination resolves to a private or reserved address.".into());
    }
    // Pin this lookup through TLS connect; redirects and proxies cannot bypass it.
    reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(host, &addresses).timeout(Duration::from_secs(20)).build()
        .map_err(|_| "Could not prepare a secure connection.".into())
}

pub(crate) async fn post(url: &str, secret: &str, delivery: &str, body: &[u8], signed: bool) -> Result<(), String> {
    let url = public_url(url)?;
    let client = public_client(&url).await?;
    let timestamp = chrono::Utc::now().timestamp().to_string();
    let signature = signature(secret.as_bytes(), &timestamp, body);
    let attempts = if signed { 3 } else { 1 };
    let mut request = client.post(url).header("Content-Type", "application/json")
        .header("X-Sona-Delivery", delivery);
    if signed {
        request = request.header("X-Sona-Timestamp", &timestamp).header("X-Sona-Signature", &signature);
    }
    let request = request.body(body.to_vec()).build().map_err(|_| "Could not prepare this delivery.")?;
    for attempt in 0..attempts {
        let result = client.execute(request.try_clone().ok_or("Could not repeat this delivery.")?).await;
        let retry = match result {
            Ok(response) if response.status().is_success() => return Ok(()),
            Ok(response) => {
                let status = response.status();
                if !(status.is_server_error() || status.as_u16() == 429) || attempt + 1 == attempts {
                    return Err(format!("The destination returned HTTP {}. Check its address and access settings.", status.as_u16()));
                }
                true
            }
            Err(_) if attempt + 1 < attempts => true,
            Err(_) => return Err("The destination did not confirm delivery. Check it before sending again.".into()),
        };
        if retry { tokio::time::sleep(Duration::from_secs(1 << attempt)).await; }
    }
    Err("The destination did not confirm delivery.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_covers_timestamp_and_exact_bytes() {
        assert_eq!(signature(b"key", "1700000000", b"{\"a\":1}"),
            "sha256=a438e398bfafc57e4396bb7fc2304422f0f768e965d073ca313cb52e22e6ad03");
        assert_ne!(signature(b"key", "1", b"{}"), signature(b"key", "2", b"{}"));
    }

    #[test]
    fn public_delivery_cannot_reach_local_networks() {
        for value in ["http://example.com", "https://localhost/hook", "https://a.ts.net/hook",
            "https://10.0.0.1/hook", "https://100.64.0.1/hook", "https://[::1]/hook",
            "https://user:password@example.com", "https://example.com/#secret"] {
            assert!(public_url(value).is_err(), "{value}");
        }
        assert!(public_url("https://hooks.zapier.com/hooks/catch/123/abc/").is_ok());
        assert!(!public_ip("169.254.169.254".parse().unwrap()));
        assert!(!public_ip("::ffff:127.0.0.1".parse().unwrap()));
        assert!(public_ip("8.8.8.8".parse().unwrap()));
        assert!(public_ip("2606:4700:4700::1111".parse().unwrap()));
    }
}
