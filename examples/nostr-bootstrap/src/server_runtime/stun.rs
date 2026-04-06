use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::time::timeout;

use super::*;
use crate::common::{
    create_stun_binding_request, local_ipv4_hint, log_stun_attempt, log_stun_result,
};
use crate::{parse_stun_url, TraversalAddress};

impl ServerRuntimeCore {
    pub async fn refresh_traversal_observation(
        &self,
        force: bool,
    ) -> Result<Option<StunObservation>> {
        if self.stun_servers.is_empty() {
            return Ok(self.stun_observation.read().await.clone());
        }

        if !force {
            let observed_at = *self.stun_observed_at.lock().await;
            if let Some(observed_at) = observed_at {
                if observed_at.elapsed() < Duration::from_millis(self.stun_refresh_ms) {
                    return Ok(self.stun_observation.read().await.clone());
                }
            }
        }

        let local_port = self.udp_socket.local_addr()?.port();
        let mut local_addresses = Vec::new();
        if let Some(ip) = local_ipv4_hint() {
            local_addresses.push(ip.to_string());
        }

        for server in &self.stun_servers {
            log_stun_attempt("server", server, local_port, &local_addresses);
            match self.probe_stun_server(server).await {
                Ok(reflexive) => {
                    log_stun_result(
                        "server",
                        server,
                        local_port,
                        &local_addresses,
                        Ok(&reflexive),
                    );
                    let obs = StunObservation {
                        server: server.clone(),
                        reflexive_address: Some(reflexive),
                        local_port,
                        local_interface_addresses: local_addresses.clone(),
                    };
                    *self.stun_observation.write().await = Some(obs.clone());
                    *self.stun_observed_at.lock().await = Some(Instant::now());
                    return Ok(Some(obs));
                }
                Err(err) => {
                    let error = err.to_string();
                    log_stun_result("server", server, local_port, &local_addresses, Err(&error));
                }
            }
        }

        let obs = StunObservation {
            server: self.stun_servers[0].clone(),
            reflexive_address: None,
            local_port,
            local_interface_addresses: local_addresses,
        };
        *self.stun_observation.write().await = Some(obs.clone());
        *self.stun_observed_at.lock().await = Some(Instant::now());
        Ok(Some(obs))
    }

    async fn probe_stun_server(&self, stun_url: &str) -> Result<LegacyEndpoint> {
        let endpoint = parse_stun_url(stun_url)?;
        let txn_id: [u8; 12] = rand::random();
        let request = create_stun_binding_request(txn_id);
        let (tx, rx) = oneshot::channel();
        self.pending_stun.lock().await.insert(txn_id, tx);
        self.udp_socket
            .send_to(&request, format!("{}:{}", endpoint.host, endpoint.port))
            .await
            .with_context(|| {
                format!(
                    "failed to send STUN request to {}:{}",
                    endpoint.host, endpoint.port
                )
            })?;

        timeout(Duration::from_millis(self.stun_timeout_ms), rx)
            .await
            .with_context(|| format!("STUN timeout waiting for {stun_url}"))?
            .context("STUN channel dropped")
    }

    pub(crate) async fn resolve_traversal_endpoint(&self) -> Result<LegacyEndpoint> {
        let obs = self.refresh_traversal_observation(false).await?;
        if let Some(obs) = obs {
            if let Some(reflexive) = obs.reflexive_address {
                return Ok(reflexive);
            }
            if let Some(first) = obs.local_interface_addresses.first() {
                return Ok(LegacyEndpoint {
                    host: first.clone(),
                    port: obs.local_port,
                });
            }
        }

        if let Some(host) = &self.public_host {
            return Ok(LegacyEndpoint {
                host: host.clone(),
                port: self.udp_socket.local_addr()?.port(),
            });
        }

        Ok(LegacyEndpoint {
            host: local_ipv4_hint()
                .unwrap_or(Ipv4Addr::new(127, 0, 0, 1))
                .to_string(),
            port: self.udp_socket.local_addr()?.port(),
        })
    }

    pub async fn local_traversal_addresses(
        &self,
    ) -> Result<(Option<TraversalAddress>, Vec<TraversalAddress>)> {
        let local_port = self.udp_socket.local_addr()?.port();
        let observation = self.refresh_traversal_observation(false).await?;
        let reflexive_address = observation
            .as_ref()
            .and_then(|obs| obs.reflexive_address.as_ref())
            .map(|endpoint| TraversalAddress {
                protocol: "udp".to_owned(),
                ip: endpoint.host.clone(),
                port: endpoint.port,
            })
            .or_else(|| {
                self.public_host.as_ref().map(|host| TraversalAddress {
                    protocol: "udp".to_owned(),
                    ip: host.clone(),
                    port: local_port,
                })
            });
        let local_addresses = observation
            .as_ref()
            .map(|obs| {
                obs.local_interface_addresses
                    .iter()
                    .map(|host| TraversalAddress {
                        protocol: "udp".to_owned(),
                        ip: host.clone(),
                        port: obs.local_port,
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| {
                local_ipv4_hint()
                    .map(|ip| {
                        vec![TraversalAddress {
                            protocol: "udp".to_owned(),
                            ip: ip.to_string(),
                            port: local_port,
                        }]
                    })
                    .unwrap_or_default()
            });
        Ok((reflexive_address, local_addresses))
    }
}
