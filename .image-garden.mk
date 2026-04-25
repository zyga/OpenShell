# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: Canonical Ltd

define snap_install_k8s_and_registry
- snap install registry
- snap install k8s --classic --channel=1.35-classic
- snap install kubectl --classic --channel=1.35
- k8s bootstrap
- k8s status --wait-ready
endef

define UBUNTU_CLOUD_INIT_USER_DATA_TEMPLATE
$(CLOUD_INIT_USER_DATA_TEMPLATE)
$(snap_install_k8s_and_registry)
endef

define DEBIAN_CLOUD_INIT_USER_DATA_TEMPLATE
$(CLOUD_INIT_USER_DATA_TEMPLATE)
- systemctl enable --now snapd.socket snapd.service snapd.apparmor.service
$(snap_install_k8s_and_registry)
packages:
- snapd
endef

define FEDORA_CLOUD_INIT_USER_DATA_TEMPLATE
$(CLOUD_INIT_USER_DATA_TEMPLATE)
- dnf install -y snapd
- systemctl enable --now snapd.socket
- sudo ln -s /var/lib/snapd/snap /snap
$(snap_install_k8s_and_registry)
endef

define CENTOS_CLOUD_INIT_USER_DATA_TEMPLATE
$(CLOUD_INIT_USER_DATA_TEMPLATE)
- yum install -y epel-release
- yum install -y snapd
- systemctl enable --now snapd.socket snapd.service
- sudo ln -s /var/lib/snapd/snap /snap
$(snap_install_k8s_and_registry)
endef
