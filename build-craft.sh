#! /bin/sh

# SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0


# Build all crafted assets - VM root filesystem, OCI, and snap.

./tasks/scripts/vm/build-rockcraft-rootfs.sh
./tasks/scripts/build-rock.sh
./tasks/scripts/build-snap.sh

