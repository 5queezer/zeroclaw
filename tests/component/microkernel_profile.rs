#[test]
fn cargo_declares_microkernel_seed_artifacts() {
    let cargo_toml = std::fs::read_to_string("Cargo.toml").expect("read root Cargo.toml");
    let parsed: toml::Value = toml::from_str(&cargo_toml).expect("parse root Cargo.toml");

    let workspace_members = parsed
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .expect("workspace.members should be an array");
    assert!(
        workspace_members
            .iter()
            .any(|member| member.as_str() == Some("crates/hrafn-sdk"))
            && workspace_members
                .iter()
                .any(|member| member.as_str() == Some("crates/hrafn-kernel")),
        "workspace should include the SDK and kernel crates used as the microkernel boundary"
    );
    assert!(
        std::path::Path::new("crates/hrafn-kernel/Cargo.toml").exists(),
        "workspace should expose a tiny kernel seed package separate from the full CLI"
    );

    let features = parsed
        .get("features")
        .and_then(toml::Value::as_table)
        .expect("root Cargo.toml should have a [features] table");
    let kernel = features
        .get("kernel")
        .and_then(toml::Value::as_array)
        .expect("root features should include a minimal kernel feature");
    assert!(
        kernel
            .iter()
            .any(|feature| feature.as_str() == Some("dep:hrafn-sdk")),
        "root kernel feature should include dep:hrafn-sdk without implying desktop integrations"
    );
    assert!(
        features.contains_key("full"),
        "root features should name the full distribution profile explicitly"
    );
}

#[test]
fn sdk_crate_stays_dependency_light() {
    let sdk_toml =
        std::fs::read_to_string("crates/hrafn-sdk/Cargo.toml").expect("read hrafn-sdk Cargo.toml");
    let parsed: toml::Value = toml::from_str(&sdk_toml).expect("parse hrafn-sdk Cargo.toml");

    let dependency_keys = dependency_table_keys(&parsed);
    for forbidden in [
        "reqwest",
        "axum",
        "ratatui",
        "rusqlite",
        "matrix-sdk",
        "wa-rs",
    ] {
        assert!(
            !dependency_keys
                .iter()
                .any(|dependency| dependency == forbidden),
            "hrafn-sdk must not depend on heavyweight integration crate {forbidden}"
        );
    }
}

fn dependency_table_keys(manifest: &toml::Value) -> Vec<String> {
    let mut keys = Vec::new();
    collect_dependency_table_keys(manifest, &mut keys);
    keys
}

fn collect_dependency_table_keys(value: &toml::Value, keys: &mut Vec<String>) {
    let Some(table) = value.as_table() else {
        return;
    };

    for (name, child) in table {
        if matches!(
            name.as_str(),
            "dependencies" | "dev-dependencies" | "build-dependencies"
        ) || name.ends_with("-dependencies")
        {
            if let Some(dependencies) = child.as_table() {
                keys.extend(dependencies.keys().cloned());
            }
        }

        collect_dependency_table_keys(child, keys);
    }
}
