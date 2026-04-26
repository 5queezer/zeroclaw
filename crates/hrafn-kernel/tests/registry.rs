use hrafn_kernel::{KernelRegistry, RegistryError};
use hrafn_sdk::{ExtensionKind, PluginManifest};

#[test]
fn registry_accepts_plugins_with_granted_permissions() {
    let mut registry = KernelRegistry::new(["network:https://openrouter.ai"].into_iter());
    let manifest = PluginManifest::new("openrouter", "0.1.0", ExtensionKind::Provider)
        .with_capability("provider.chat")
        .with_permission("network:https://openrouter.ai");

    registry.register(manifest).expect("register plugin");

    let plugin = registry.get("openrouter").expect("registered plugin");
    assert_eq!(plugin.kind, ExtensionKind::Provider);
    assert_eq!(plugin.capabilities[0].name, "provider.chat");
}

#[test]
fn registry_rejects_plugins_with_ungranted_permissions() {
    let mut registry = KernelRegistry::new(std::iter::empty::<&str>());
    let manifest = PluginManifest::new("shell", "0.1.0", ExtensionKind::Tool)
        .with_capability("tool.execute")
        .with_permission("shell:exec");

    let err = registry.register(manifest).expect_err("permission denied");

    assert_eq!(
        err,
        RegistryError::PermissionDenied {
            plugin: "shell".into(),
            permission: "shell:exec".into(),
        }
    );
}

#[test]
fn registry_rejects_duplicate_plugin_names() {
    let mut registry = KernelRegistry::default();
    let manifest = PluginManifest::new("duplicate", "0.1.0", ExtensionKind::Tool);

    registry.register(manifest.clone()).expect("first register");
    let err = registry.register(manifest).expect_err("duplicate rejected");

    assert_eq!(err, RegistryError::DuplicatePlugin("duplicate".into()));
}
