// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use kube::{
    Client,
    api::{Api, DynamicObject, Patch, PatchParams},
    discovery::{Discovery, Scope},
};
use miette::{Context, IntoDiagnostic, Result};
use serde::Deserialize;

pub async fn apply_yaml(client: Client, yaml: &str, namespace: &str) -> Result<()> {
    let mut attempt = 0;
    let discovery = loop {
        match Discovery::new(client.clone()).run().await {
            Ok(d) => break d,
            Err(e) => {
                attempt += 1;
                if attempt >= 10 {
                    return Err(e).into_diagnostic();
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    };

    let mut objects = Vec::new();
    for document in serde_yml::Deserializer::from_str(yaml) {
        let value = serde_yml::Value::deserialize(document).into_diagnostic()?;
        if value.is_null() {
            continue;
        }

        objects.push(serde_yml::from_value::<DynamicObject>(value).into_diagnostic()?);
    }

    for obj in objects {
        let types = obj.types.as_ref().unwrap();
        let (ar, caps) = discovery
            .resolve_gvk(&kube::api::GroupVersionKind::try_from(types).unwrap())
            .unwrap();

        let target_namespace = obj.metadata.namespace.as_deref().unwrap_or(namespace);
        let api: Api<DynamicObject> = if caps.scope == Scope::Namespaced {
            Api::namespaced_with(client.clone(), target_namespace, &ar)
        } else {
            Api::all_with(client.clone(), &ar)
        };

        let name = obj.metadata.name.clone().unwrap();
        let params = PatchParams::apply("openshell-bootstrap").force();
        tracing::info!(
            "Applying {}/{} (namespace: {})",
            types.kind,
            name,
            target_namespace
        );
        api.patch(&name, &params, &Patch::Apply(&obj))
            .await
            .into_diagnostic()
            .context(format!("Failed to apply {} {}", types.kind, name))?;
    }
    Ok(())
}
