use std::net::IpAddr;

use axum::{
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use axum_client_ip::ClientIp;

pub async fn force_local_request<B>(
    ClientIp(ip): ClientIp,
    req: Request<B>,
    next: Next<B>,
) -> Result<Response, StatusCode> {
    let real_ip = if let IpAddr::V6(ip6) = ip {
        match ip6.to_ipv4_mapped() {
            Some(ip4) => IpAddr::V4(ip4),
            None => IpAddr::V6(ip6),
        }
    } else {
        ip
    };
    tracing::debug!("ip: {:?} real_remote_ip {:?}", ip, real_ip);

    if !real_ip.is_loopback() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(req).await)
}
