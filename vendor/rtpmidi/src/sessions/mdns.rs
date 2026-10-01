#[cfg(feature = "mdns")]
pub fn advertise_mdns(instance_name: &str, port: u16) -> Result<mdns_sd::ServiceDaemon, mdns_sd::Error> {
    use mdns_sd::{ServiceDaemon, ServiceInfo};

    let mdns = ServiceDaemon::new()?;
    let service_type = "_apple-midi._udp.local.";
    let ip = local_ip_address::local_ip().expect("Failed to get local IP address").to_string();

    let raw_hostname = hostname::get().expect("Failed to get hostname").to_string_lossy().to_string();
    let hostname = format!("{raw_hostname}.local.");
    let service = ServiceInfo::new(service_type, instance_name, &hostname, ip, port, None)?;
    mdns.register(service)?;

    Ok(mdns)
}
