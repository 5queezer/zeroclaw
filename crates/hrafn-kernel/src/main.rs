use hrafn_kernel::KernelRegistry;
use hrafn_sdk::{ExtensionKind, PluginManifest, SDK_PROTOCOL_VERSION};

fn main() {
    let manifest = PluginManifest::new(
        "hrafn-kernel",
        env!("CARGO_PKG_VERSION"),
        ExtensionKind::Runtime,
    )
    .with_capability("kernel.registry")
    .with_permission("kernel.permissions");
    let mut registry = KernelRegistry::new(["kernel.permissions"]);
    registry
        .register(manifest.clone())
        .expect("kernel manifest should register");

    println!(
        "name={} version={} sdk_protocol={} plugins={}",
        manifest.name,
        manifest.version,
        SDK_PROTOCOL_VERSION,
        registry.len()
    );
}
