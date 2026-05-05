// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use k8s_openapi::ByteString;
use k8s_openapi::api::core::v1::{Namespace, Secret};
use kube::{
    Api, Client,
    api::{Patch, PatchParams, PostParams},
    core::ObjectMeta,
};
use miette::{Context, IntoDiagnostic, Result};
use std::collections::BTreeMap;

use crate::constants::{
    CLIENT_TLS_SECRET_NAME, SERVER_CLIENT_CA_SECRET_NAME, SERVER_TLS_SECRET_NAME,
    SSH_HANDSHAKE_SECRET_NAME,
};
use crate::pki;
use std::io::Read;

pub async fn init_external_cluster(
    gateway_name: &str,
    namespace: &str,
    registry: &str,
    registry_username: Option<&str>,
    registry_token: Option<&str>,
    registry_authfile: Option<&str>,
) -> Result<()> {
    let client = Client::try_default()
        .await
        .into_diagnostic()
        .context("failed to create Kubernetes client")?;

    // 1. Create namespace
    ensure_namespace(client.clone(), namespace).await?;

    // 2. Reconcile PKI
    let bundle = if let Some(existing_bundle) =
        crate::reconcile::try_load_pki(client.clone(), namespace).await?
    {
        tracing::info!("Using existing PKI certificates from cluster");
        existing_bundle
    } else {
        tracing::info!("Generating new PKI certificates");
        let new_bundle = pki::generate_pki(&[]).context("failed to generate PKI")?;

        // 3. Create all TLS secrets in Kubernetes
        ensure_client_tls_secret(client.clone(), namespace, &new_bundle).await?;
        ensure_server_tls_secret(client.clone(), namespace, &new_bundle).await?;
        ensure_server_client_ca_secret(client.clone(), namespace, &new_bundle).await?;

        // Helm will automatically roll out the StatefulSet if secrets change (if the chart has checksum annotations)
        // or we can rely on standard Helm behavior. For now, no explicit restart is needed.

        new_bundle
    };

    // 4. Create SSH handshake secret in Kubernetes
    ensure_ssh_handshake_secret(client.clone(), namespace).await?;

    // 5. Deploy agent-sandbox CRD and controller
    let sandbox_manifest = include_str!("../../../deploy/kube/manifests/agent-sandbox.yaml");
    tracing::info!("Applying agent-sandbox manifests");
    crate::apply::apply_yaml(client.clone(), sandbox_manifest, namespace).await?;

    // 6. Adopt legacy Server-Side Applied resources so Helm can manage them
    tracing::info!("Adopting legacy resources for Helm");
    crate::reconcile::adopt_legacy_resources(client.clone(), namespace).await?;

    // 7. Deploy Gateway via Helm
    tracing::info!("Deploying gateway via Helm");
    let snap_dir = std::env::var("SNAP").unwrap_or_else(|_| ".".to_string());

    // Determine the path to the helm chart depending on if we are running in the snap or locally
    let chart_path = if snap_dir == "." {
        "../../../deploy/helm/openshell".to_string()
    } else {
        format!("{snap_dir}/helm-charts/openshell")
    };

    let helm_bin = if snap_dir == "." {
        "helm".to_string()
    } else {
        format!("{snap_dir}/bin/helm")
    };

    let gateway_tar = format!("{snap_dir}/images/gateway.tar");
    let supervisor_tar = format!("{snap_dir}/images/supervisor.tar");
    let core_rock = format!("{snap_dir}/images/openshell-core.rock");

    let (has_bundled_images, is_core_rock) = if std::path::Path::new(&core_rock).exists() {
        (true, true)
    } else if std::path::Path::new(&gateway_tar).exists()
        && std::path::Path::new(&supervisor_tar).exists()
    {
        (true, false)
    } else {
        (false, false)
    };

    let mut images_loaded = false;
    let target_registry = registry;

    if has_bundled_images {
        tracing::info!(
            "Found bundled Docker images/ROCKs in the snap. Attempting to push to registry at {}...",
            target_registry
        );

        let mut push_success = false;

        let snap_user_data = std::env::var("SNAP_USER_DATA").unwrap_or_else(|_| "/tmp".to_string());

        let (authfile_path, _tmp_dir_guard) = if let Some(path) = registry_authfile {
            (std::path::PathBuf::from(path), None)
        } else {
            // Validate and resolve credentials
            let effective_username = match (registry_username, registry_token) {
                (Some(u), Some(_)) => Some(u),
                (None, Some(_)) => Some("__token__"),
                (Some(_), None) => {
                    return Err(miette::miette!(
                        "A registry token/password must be provided when a username is specified."
                    ));
                }
                (None, None) => None,
            };

            if let Some(token) = registry_token {
                // Create tempdir for secure skopeo execution
                let tmp_dir = tempfile::tempdir()
                    .into_diagnostic()
                    .context("Failed to create temporary directory for secure auth")?;
                let authfile = tmp_dir.path().join("auth.json");

                let u = effective_username.unwrap();
                let mut login_cmd = std::process::Command::new("skopeo");
                login_cmd.env("CONTAINERS_REGISTRIES_CONF", "/dev/null");
                login_cmd
                    .arg("login")
                    .arg("--authfile")
                    .arg(authfile.as_os_str())
                    .arg("--username")
                    .arg(u)
                    .arg("--password-stdin")
                    .arg(target_registry);

                // Pipe token to stdin
                login_cmd.stdin(std::process::Stdio::piped());
                login_cmd.stdout(std::process::Stdio::null());
                login_cmd.stderr(std::process::Stdio::piped());

                let mut child = login_cmd
                    .spawn()
                    .into_diagnostic()
                    .context("Failed to spawn skopeo login")?;
                if let Some(mut stdin) = child.stdin.take() {
                    use std::io::Write;
                    stdin.write_all(token.as_bytes()).into_diagnostic()?;
                }

                let mut stderr_output = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    use std::io::Read;
                    let _ = stderr.read_to_string(&mut stderr_output);
                }

                let status = child
                    .wait()
                    .into_diagnostic()
                    .context("Failed to wait on skopeo login")?;
                if !status.success() {
                    return Err(miette::miette!(
                        "Failed to authenticate with registry using skopeo login. Error:\n{}",
                        stderr_output.trim()
                    ));
                }
                (authfile, Some(tmp_dir))
            } else {
                (std::path::PathBuf::new(), None)
            }
        };

        let has_authfile = registry_authfile.is_some() || registry_token.is_some();

        if is_core_rock {
            let target_image = format!("{target_registry}/openshell-core:local");

            let mut cmd = std::process::Command::new("skopeo");
            cmd.env("CONTAINERS_REGISTRIES_CONF", "/dev/null");
            cmd.arg("--insecure-policy")
                .arg("copy")
                .arg("--dest-tls-verify=false");

            if has_authfile {
                cmd.arg("--authfile").arg(authfile_path.as_os_str());
            }

            let output = cmd
                .arg(format!("--tmpdir={snap_user_data}"))
                .arg(format!("oci-archive:{core_rock}"))
                .arg(format!("docker://{target_image}"))
                .output();

            match output {
                Ok(out) if out.status.success() => {
                    tracing::info!("Successfully pushed unified openshell-core to local registry.");
                    push_success = true;
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    tracing::warn!(
                        "Failed to push openshell-core image. Skopeo output:\n{}",
                        stderr.trim()
                    );
                }
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        tracing::warn!(
                            "'skopeo' command not found. Please install skopeo to push images."
                        );
                    } else {
                        tracing::warn!("Failed to execute skopeo: {}", e);
                    }
                }
            }
        } else {
            let target_gateway = format!("{target_registry}/openshell-gateway:dev");
            let target_supervisor = format!("{target_registry}/openshell-supervisor:dev");

            let mut cmd1 = std::process::Command::new("skopeo");
            cmd1.env("CONTAINERS_REGISTRIES_CONF", "/dev/null");
            cmd1.arg("--insecure-policy")
                .arg("copy")
                .arg("--dest-tls-verify=false");

            if has_authfile {
                cmd1.arg("--authfile").arg(authfile_path.as_os_str());
            }

            let o1 = cmd1
                .arg(format!("--tmpdir={snap_user_data}"))
                .arg(format!("oci-archive:{gateway_tar}"))
                .arg(format!("docker://{target_gateway}"))
                .output();

            let mut cmd2 = std::process::Command::new("skopeo");
            cmd2.env("CONTAINERS_REGISTRIES_CONF", "/dev/null");
            cmd2.arg("--insecure-policy")
                .arg("copy")
                .arg("--dest-tls-verify=false");

            if has_authfile {
                cmd2.arg("--authfile").arg(authfile_path.as_os_str());
            }

            let o2 = cmd2
                .arg(format!("--tmpdir={snap_user_data}"))
                .arg(format!("oci-archive:{supervisor_tar}"))
                .arg(format!("docker://{target_supervisor}"))
                .output();

            match (o1, o2) {
                (Ok(out1), Ok(out2)) if out1.status.success() && out2.status.success() => {
                    tracing::info!("Successfully pushed gateway and supervisor to registry.");
                    push_success = true;
                }
                (r1, r2) => {
                    if let Ok(out1) = &r1 {
                        if !out1.status.success() {
                            tracing::warn!(
                                "Failed to push gateway image. Skopeo output:\n{}",
                                String::from_utf8_lossy(&out1.stderr).trim()
                            );
                        }
                    } else if let Err(e) = &r1 {
                        if e.kind() == std::io::ErrorKind::NotFound {
                            tracing::warn!(
                                "'skopeo' command not found. Please install skopeo to push images."
                            );
                        } else {
                            tracing::warn!("Failed to execute skopeo (gateway): {}", e);
                        }
                    }
                    if let Ok(out2) = &r2 {
                        if !out2.status.success() {
                            tracing::warn!(
                                "Failed to push supervisor image. Skopeo output:\n{}",
                                String::from_utf8_lossy(&out2.stderr).trim()
                            );
                        }
                    } else if let Err(e) = &r2 {
                        if e.kind() == std::io::ErrorKind::NotFound {
                            tracing::warn!(
                                "'skopeo' command not found. Please install skopeo to push images."
                            );
                        } else {
                            tracing::warn!("Failed to execute skopeo (supervisor): {}", e);
                        }
                    }
                }
            }
        }

        if push_success {
            images_loaded = true;
        } else {
            tracing::warn!("Failed to push bundled images to the registry.");
            tracing::warn!(
                "For local development, please run \"sudo snap install registry\" and use \"localhost:5000\", or point to your preferred registry with $OPENSHELL_REGISTRY or --registry."
            );

            // We exit early because without the local images available in the registry,
            // the Helm deployment will fail to pull them and the pods will hang in ImagePullBackOff.
            return Err(miette::miette!(
                "Local registry is not available or unreachable at {}.",
                target_registry
            ));
        }
    }

    let mut helm_cmd = std::process::Command::new(helm_bin);
    helm_cmd
        .arg("upgrade")
        .arg("--install")
        .arg("gateway")
        .arg(&chart_path)
        .arg("--namespace")
        .arg(namespace)
        .arg("--wait")
        .arg("--timeout")
        .arg("2m");

    if images_loaded {
        if is_core_rock {
            helm_cmd
                .arg("--set")
                .arg(format!("image.repository={target_registry}/openshell-core"))
                .arg("--set")
                .arg("image.tag=local")
                .arg("--set")
                .arg("image.pullPolicy=Always")
                .arg("--set")
                .arg(format!(
                    "supervisorImage={target_registry}/openshell-core:local"
                ))
                .arg("--set")
                .arg("supervisorImagePullPolicy=Always");
        } else {
            helm_cmd
                .arg("--set")
                .arg(format!(
                    "image.repository={target_registry}/openshell-gateway"
                ))
                .arg("--set")
                .arg("image.tag=dev")
                .arg("--set")
                .arg("image.pullPolicy=Always")
                .arg("--set")
                .arg(format!(
                    "supervisorImage={target_registry}/openshell-supervisor:dev"
                ))
                .arg("--set")
                .arg("supervisorImagePullPolicy=Always");
        }
    }

    let status = helm_cmd
        .status()
        .into_diagnostic()
        .context("Failed to execute helm upgrade")?;

    if !status.success() {
        return Err(miette::miette!(
            "Helm upgrade failed with status: {}",
            status
        ));
    }

    // 6. Save client TLS materials locally for CLI to connect
    let home_dir =
        dirs::home_dir().ok_or_else(|| miette::miette!("Could not find home directory"))?;
    let mtls_dir = home_dir
        .join(".config")
        .join("openshell")
        .join("gateways")
        .join(gateway_name)
        .join("mtls");
    std::fs::create_dir_all(&mtls_dir).into_diagnostic()?;

    std::fs::write(mtls_dir.join("tls.crt"), &bundle.client_cert_pem).into_diagnostic()?;
    std::fs::write(mtls_dir.join("tls.key"), &bundle.client_key_pem).into_diagnostic()?;
    std::fs::write(mtls_dir.join("ca.crt"), &bundle.ca_cert_pem).into_diagnostic()?;

    // 7. Save gateway metadata and set active gateway
    let metadata = crate::metadata::create_gateway_metadata(
        gateway_name,
        None,
        30051, // NodePort from gateway.yaml
    );
    crate::metadata::store_gateway_metadata(gateway_name, &metadata)?;
    crate::metadata::save_active_gateway(gateway_name)?;

    Ok(())
}

async fn ensure_namespace(client: Client, namespace: &str) -> Result<()> {
    let namespaces: Api<Namespace> = Api::all(client);

    let ns = Namespace {
        metadata: ObjectMeta {
            name: Some(namespace.to_string()),
            ..Default::default()
        },
        ..Default::default()
    };

    let params = PatchParams::apply("openshell-bootstrap").force();
    namespaces
        .patch(namespace, &params, &Patch::Apply(&ns))
        .await
        .into_diagnostic()
        .context("failed to apply namespace")?;

    Ok(())
}

async fn ensure_client_tls_secret(
    client: Client,
    namespace: &str,
    bundle: &pki::PkiBundle,
) -> Result<()> {
    let secrets: Api<Secret> = Api::namespaced(client, namespace);

    let mut data = BTreeMap::new();
    data.insert(
        "tls.crt".to_string(),
        ByteString(bundle.client_cert_pem.as_bytes().to_vec()),
    );
    data.insert(
        "tls.key".to_string(),
        ByteString(bundle.client_key_pem.as_bytes().to_vec()),
    );
    data.insert(
        "ca.crt".to_string(),
        ByteString(bundle.ca_cert_pem.as_bytes().to_vec()),
    );

    let secret = Secret {
        metadata: ObjectMeta {
            name: Some(CLIENT_TLS_SECRET_NAME.to_string()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        type_: Some("Opaque".to_string()),
        data: Some(data),
        ..Default::default()
    };

    let params = PatchParams::apply("openshell-bootstrap").force();
    secrets
        .patch(CLIENT_TLS_SECRET_NAME, &params, &Patch::Apply(&secret))
        .await
        .into_diagnostic()
        .context("failed to apply client TLS secret")?;

    Ok(())
}

async fn ensure_ssh_handshake_secret(client: Client, namespace: &str) -> Result<()> {
    let secrets: Api<Secret> = Api::namespaced(client, namespace);

    // Check if it already exists
    if secrets.get(SSH_HANDSHAKE_SECRET_NAME).await.is_ok() {
        return Ok(());
    }

    let mut buf = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .into_diagnostic()
        .context("failed to open /dev/urandom")?
        .read_exact(&mut buf)
        .into_diagnostic()
        .context("failed to read from /dev/urandom")?;

    let mut secret_val = String::with_capacity(64);
    for b in buf {
        use std::fmt::Write;
        write!(&mut secret_val, "{b:02x}").unwrap();
    }
    let mut data = BTreeMap::new();
    data.insert(
        "secret".to_string(),
        ByteString(secret_val.as_bytes().to_vec()),
    );

    let secret = Secret {
        metadata: ObjectMeta {
            name: Some(SSH_HANDSHAKE_SECRET_NAME.to_string()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        type_: Some("Opaque".to_string()),
        data: Some(data),
        ..Default::default()
    };

    secrets
        .create(&PostParams::default(), &secret)
        .await
        .into_diagnostic()
        .context("failed to create ssh handshake secret")?;

    Ok(())
}

async fn ensure_server_tls_secret(
    client: Client,
    namespace: &str,
    bundle: &pki::PkiBundle,
) -> Result<()> {
    let secrets: Api<Secret> = Api::namespaced(client, namespace);
    let mut data = BTreeMap::new();
    data.insert(
        "tls.crt".to_string(),
        ByteString(bundle.server_cert_pem.as_bytes().to_vec()),
    );
    data.insert(
        "tls.key".to_string(),
        ByteString(bundle.server_key_pem.as_bytes().to_vec()),
    );

    let secret = Secret {
        metadata: ObjectMeta {
            name: Some(SERVER_TLS_SECRET_NAME.to_string()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        type_: Some("kubernetes.io/tls".to_string()),
        data: Some(data),
        ..Default::default()
    };

    let params = PatchParams::apply("openshell-bootstrap").force();
    secrets
        .patch(SERVER_TLS_SECRET_NAME, &params, &Patch::Apply(&secret))
        .await
        .into_diagnostic()
        .context("failed to apply server TLS secret")?;

    Ok(())
}

async fn ensure_server_client_ca_secret(
    client: Client,
    namespace: &str,
    bundle: &pki::PkiBundle,
) -> Result<()> {
    let secrets: Api<Secret> = Api::namespaced(client, namespace);
    let mut data = BTreeMap::new();
    data.insert(
        "ca.crt".to_string(),
        ByteString(bundle.ca_cert_pem.as_bytes().to_vec()),
    );

    let secret = Secret {
        metadata: ObjectMeta {
            name: Some(SERVER_CLIENT_CA_SECRET_NAME.to_string()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        type_: Some("Opaque".to_string()),
        data: Some(data),
        ..Default::default()
    };

    let params = PatchParams::apply("openshell-bootstrap").force();
    secrets
        .patch(
            SERVER_CLIENT_CA_SECRET_NAME,
            &params,
            &Patch::Apply(&secret),
        )
        .await
        .into_diagnostic()
        .context("failed to apply server client CA secret")?;

    Ok(())
}
