# OpenShell Snap

This branch is a PoC of a simpler, more secure and easily managed OpenShell
install, update and distribution experience.

The goal of this UX is to make it trivial for any user to get OpenShell and
start working with it locally using VM sandboxes.

If they want, they can also install a local K8s and registry, deploy the
gateway and run sandboxes on that. The same commands work with a remote
Kubernetes cluster.

The snap can use Podman locally too.

## Get started with local MicroVM sandboxes:

```
sudo snap install openshell
openshell sandbox create
```

That's it. Nothing else is needed if your machine supports KVM. You are now
able to create VM sandboxes locally.

## Add a K8s cluster as a place to run sandboxes

You might want to use Kubernetes to host sandboxes, particularly if your
machine does not support KVM (for example a cloud VM that does not have nested
virt support).

The `openshell` CLI can deploy its gateway to K8s directly, given a registry
and a kubeconfig file. The command to deploy is:

```
  openshell cluster init --registry <endpoint> --kubeconfig <filename>
                        [[--registry-username <user>] [--registry-token <token>]] [--registry-authfile <filename>]
```

For example, this command will deploy to a local K8s cluster on Ubuntu:

```
sudo cat /etc/kubernetes/admin.conf | openshell cluster init --gateway local-k8s --registry localhost:5000 --kubeconfig -
```

This will verify both the registry and the Kubernetes credential, push images to
the registry, deploy a daemonset that provides a custom AppArmor profile for
the supervisor, and run the gateway service. It will then register this client
so that it can create sandboxes on that Kubernetes.

Here is the simplest way to setup K8s on Ubuntu:

```
sudo snap install k8s --classic --channel=1.35-classic            # The tested version is 1.35
sudo snap install registry

sudo k8s bootstrap
sudo k8s status --wait-ready
```

At this stage you should have a K8s running locally, with the admin
configuration in `/etc/kubernetes/admin.conf` and a Docker registry running on
localhost:5000.  If you want, test it out:

```
sudo snap install kubectl --classic
sudo kubectl get nodes --kubeconfig=/etc/kubernetes/admin.conf
```

We can deploy the OpenShell gateway on that cluster:

```
sudo cat /etc/kubernetes/admin.conf | openshell cluster init --gateway local-k8s --registry localhost:5000 --kubeconfig -
openshell gateway select --list
openshell sandbox create
```

## Build it

You need `rockcraft` and `snapcraft` to build the rock (OCI) and snap respectively.

```
sudo snap install snapcraft --classic
sudo snap install rockcraft --classic
```

We build three artifacts in order:

```
./tasks/scripts/vm/build-rockcraft-rootfs.sh
./tasks/scripts/build-rock.sh
./tasks/scripts/build-snap.sh
```

This will build a rock (docker image) and then a snap which bundles that rock.
Bundling the rock is a short term thing, we will shortly be able to attach the
rock as a resource for the snap, which can be pulled dynamically and updated
independently.

You should now see `openshell_<version>_<architecture>.snap` in the top-level
directory. Since it has just been built locally it is not signed, so Ubuntu
will not trust it by default. You need to install it with the `--dangerous`
flag to acknowledge that:

```
sudo snap install ./openshell_<v>_<arch>.snap --dangerous
```


# TODO

 - tests
 - slim down VM rootfs - chiselled rock?
 - figure out if we still need kube-config
 - properly remove K3s from image-building tangle
 - merge `openshell cluster init` and `openshell podman init` into `openshell gateway start` that takes all the right flavour-specific parameters
 - figure out if podman socket can be made visible to gateway
 - do better than process-control
 - documentation!


# Developer Setup

```
git clone...
cd OpenShell

sudo snap install lxd                            # Used for clean image builds
sudo snap install rockcraft --classic            # Used for OCI and rootfs builds, in clean LXC containers
sudo snap install snapcraft --classic            # Used for snap builds, in clean LXC containers
sudo apt install umoci fakeroot

sudo lxd init                                    # Say yes to the defaults, it will give you a ZFS backed cache for developer iteration
```

## First build

```
./tasks/scripts/vm/build-rockcraft-rootfs.sh     # This will build the sandbox VM rootfs using rockcraft
./tasks/scripts/build-rock.sh                    # This will build the OCI used for gateway and supervisor
./tasks/scripts/build-snap.sh                    # THis will build the snap; it needs both the VM rootfs and the rock above
```

## Install developer build

```
sudo snap install ./openshell_<version>_<arch>.snap --dangerous
```

## Sanity check

You want to be using the ZFS storage driver for LXC for faster build iteration:

```
$ lxc storage list
+---------+--------+--------------------------------------------+-------------+---------+---------+
|  NAME   | DRIVER |                   SOURCE                   | DESCRIPTION | USED BY |  STATE  |
+---------+--------+--------------------------------------------+-------------+---------+---------+
| default | zfs    | /var/snap/lxd/common/lxd/disks/default.img |             | 5       | CREATED |
+---------+--------+--------------------------------------------+-------------+---------+---------+
```


# WALKTHROUGH

sudo snap install openshell
openshell gateway list
openshell sandbox list
openshell sandbox create           # should error without KVM unless we get auto-connect
sudo snap connect openshell:kvm
openshell sandbox create           # should now succeed
openshell sandbox list

