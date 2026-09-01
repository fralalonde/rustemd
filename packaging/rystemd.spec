# rystemd RPM spec. Consumed by .github/workflows/release.yml, which builds
# the release binaries first and points rpmbuild at them via `--define`.
#
#   rpmbuild -bb packaging/rystemd.spec \
#       --define "_topdir $PWD/rpm" \
#       --define "version 0.2.0" \
#       --define "_bindir_stage $PWD/target/<triple>/release"
Name:           rystemd
Version:        %{version}
Release:        1
Summary:        systemd init reimplementation: unit manager + systemctl-compatible CLI
License:        MIT
URL:            https://github.com/rystemd/rystemd

%description
rystemd is a systemd init reimplementation in Rust: a drop-in systemctl
replacement with unit files, user services, and timers. Ships the rystemd
manager daemon, the rystemctl systemctl-compatible CLI, and the rystemd-tui
terminal client.

%install
install -d %{buildroot}%{_bindir}
install -m 0755 %{_bindir_stage}/rystemd %{buildroot}%{_bindir}/rystemd
install -m 0755 %{_bindir_stage}/rystemctl %{buildroot}%{_bindir}/rystemctl
install -m 0755 %{_bindir_stage}/rystemd-tui %{buildroot}%{_bindir}/rystemd-tui

%files
%{_bindir}/rystemd
%{_bindir}/rystemctl
%{_bindir}/rystemd-tui

%changelog
* Sun Aug 23 2026 Francis Lalonde <6190260+fralalonde@users.noreply.github.com> - %{version}-1
- Initial release
