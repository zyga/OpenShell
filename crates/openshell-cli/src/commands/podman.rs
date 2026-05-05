// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use miette::{IntoDiagnostic, Result, WrapErr, miette};
use owo_colors::OwoColorize;
use std::env;
use std::path::PathBuf;

use openshell_driver_podman::client::PodmanClient;

/// Initialize Podman to serve as a compute driver for `OpenShell`.
pub async fn init() -> Result<()> {
    println!("{}", "Initializing Podman for OpenShell...".bold());

    // 1. Check socket path
    let socket_path = env::var("OPENSHELL_PODMAN_SOCKET").unwrap_or_else(|_| {
        if env::var("SNAP").is_ok() {
            "tcp://127.0.0.1:8888".to_string()
        } else if let Ok(snap_user_common) = env::var("SNAP_USER_COMMON") {
            format!("{snap_user_common}/podman.sock")
        } else if let Ok(home) = env::var("HOME") {
            format!("{home}/snap/openshell/common/podman.sock")
        } else {
            "/var/snap/openshell/common/podman.sock".to_string()
        }
    });

    let socket_path_buf = PathBuf::from(&socket_path);
    let path_str = socket_path_buf.to_string_lossy();

    if !path_str.starts_with("tcp://") && !socket_path_buf.exists() {
        println!("\n{}", "Podman socket not found!".red().bold());
        println!("Expected socket at: {socket_path}");

        if env::var("SNAP").is_ok() {
            println!(
                "\nIf you are running OpenShell as a snap, AppArmor prevents access to external Unix sockets."
            );
            println!("You must configure Podman to listen on TCP instead:");
            println!("  podman system service -t 0 tcp://127.0.0.1:8888 &");
            println!("\nThen set OPENSHELL_PODMAN_SOCKET=tcp://127.0.0.1:8888");
        } else {
            println!(
                "\nIf you are running OpenShell natively, ensure your podman daemon is running."
            );
        }
        return Err(miette!("Podman socket not found at {}", socket_path));
    }

    println!("✓ Found Podman socket at: {socket_path}");

    // 2. Verify connectivity
    let client = PodmanClient::new(socket_path_buf.clone());
    client
        .ping()
        .await
        .into_diagnostic()
        .wrap_err("Failed to connect to Podman daemon")?;
    println!("✓ Successfully connected to Podman daemon");

    // 3. Load the OCI archive rock using skopeo
    // The rock is located at $SNAP/images/openshell-core.rock
    let snap_dir = env::var("SNAP").unwrap_or_else(|_| ".".to_string());
    let rock_path = PathBuf::from(snap_dir)
        .join("images")
        .join("openshell-core.rock");

    if rock_path.exists() {
        println!("Loading sandbox images from {}...", rock_path.display());
        println!("Loading sandbox images into Podman via API...");

        match client.load_image_archive(&rock_path).await {
            Ok(name) => {
                println!("✓ Successfully loaded image: {name}");

                // Tag it as openshell/supervisor:latest
                if let Err(e) = client
                    .tag_image(&name, "openshell/supervisor", "latest")
                    .await
                {
                    println!(
                        "{}",
                        format!("Warning: Failed to tag image as openshell/supervisor:latest: {e}")
                            .yellow()
                    );
                } else {
                    println!("✓ Tagged image as openshell/supervisor:latest");
                }

                // Tag it as openshell/base:latest
                if let Err(e) = client.tag_image(&name, "openshell/base", "latest").await {
                    println!(
                        "{}",
                        format!("Warning: Failed to tag image as openshell/base:latest: {e}")
                            .yellow()
                    );
                } else {
                    println!("✓ Tagged image as openshell/base:latest");
                }
                println!("✓ Sandbox images loaded into Podman");
            }
            Err(e) => {
                return Err(miette!("Failed to load OCI archive into Podman: {}", e));
            }
        }
    } else {
        println!(
            "{}",
            "Warning: openshell-core.rock not found locally. Skipping image load.".yellow()
        );
    }

    // 4. Print configuration instructions
    println!("\n{}", "Podman initialization complete!".green().bold());
    println!("To configure OpenShell to use the Podman compute driver:");

    if env::var("SNAP").is_ok() {
        println!("\nSince you are running OpenShell as a snap, run:");
        println!("  sudo snap set openshell driver=podman podman-socket=\"{socket_path}\"");
    } else {
        println!("\nSince you are running OpenShell natively, ensure you start the gateway with:");
        println!(
            "  OPENSHELL_PODMAN_SOCKET=\"{socket_path}\" openshell gateway start --compute-driver podman"
        );
        println!("\nOr add this to your config.toml:");
        println!("  compute_drivers = [\"podman\"]");
        println!("  # And set OPENSHELL_PODMAN_SOCKET in your environment");
    }

    Ok(())
}
