# Registry namespace the default image comes from (ghcr.io/<image_ns>);
# override with rpmbuild --define 'image_ns myorg' to ship a different default.
%global image_ns fitadevsrl

Name:           podman-lru
Version:        0.1.0
Release:        1%{?dist}
Summary:        Evict least recently used podman images to keep storage under budget
License:        MIT
URL:            https://github.com/fitadevsrl/podman-lru
Source0:        %{name}-%{version}.tar.gz
BuildArch:      noarch

BuildRequires:  systemd-rpm-macros
Requires:       podman >= 4.9
Requires:       systemd
%{?systemd_requires}

%description
podman-lru runs as a container on a build host, follows podman events through
the libpod API socket and removes the least recently used images and
intermediate layers whenever image storage exceeds a configured byte budget.

This package installs the systemd unit and its configuration. The container
image itself is pulled from the registry named in /etc/sysconfig/podman-lru
(GitHub Container Registry by default).

%prep
%autosetup

%build
# Nothing to build: the daemon ships as a container image.

%install
install -D -m 0644 packaging/podman-lru.service %{buildroot}%{_unitdir}/podman-lru.service
install -D -m 0644 packaging/podman-lru.sysconfig %{buildroot}%{_sysconfdir}/sysconfig/podman-lru
sed -i 's|^PODMAN_LRU_IMAGE=.*|PODMAN_LRU_IMAGE=%{image_ns}/podman-lru:%{version}|' \
    %{buildroot}%{_sysconfdir}/sysconfig/podman-lru
install -d -m 0755 %{buildroot}%{_sharedstatedir}/podman-lru

%post
%systemd_post podman-lru.service
# The daemon talks to podman through its API socket.
systemctl enable --now podman.socket >/dev/null 2>&1 || :

%preun
%systemd_preun podman-lru.service

%postun
%systemd_postun_with_restart podman-lru.service

%files
%license LICENSE
%doc README.md USER.md
%{_unitdir}/podman-lru.service
%config(noreplace) %{_sysconfdir}/sysconfig/podman-lru
%dir %{_sharedstatedir}/podman-lru

%changelog
* Mon Nov 09 2026 Adrien Horgnies - 0.1.0-1
- Initial package
