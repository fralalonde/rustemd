# rustemd RPM spec. Consumed by .github/workflows/release.yml, which builds
# the release binaries first and points rpmbuild at them via `--define`.
#
#   rpmbuild -bb packaging/rustemd.spec \
#       --define "_topdir $PWD/rpm" \
#       --define "version 0.2.0" \
#       --define "_bindir_stage $PWD/target/<triple>/release"
Name:           rustemd
Version:        %{version}
Release:        1
Summary:        systemd init reimplementation: unit manager + systemctl-compatible CLI
License:        MIT
URL:            https://github.com/fralalonde/rustemd

%description
rustemd is a systemd init reimplementation in Rust: a drop-in systemctl
replacement with unit files, user services, and timers. Ships the rustemd
manager daemon, the rustemctl systemctl-compatible CLI, and the rustemd-tui
terminal client.

%install
install -d %{buildroot}%{_bindir}
install -m 0755 %{_bindir_stage}/rustemd %{buildroot}%{_bindir}/rustemd
install -m 0755 %{_bindir_stage}/rustemctl %{buildroot}%{_bindir}/rustemctl
install -m 0755 %{_bindir_stage}/rustemd-tui %{buildroot}%{_bindir}/rustemd-tui

%files
%{_bindir}/rustemd
%{_bindir}/rustemctl
%{_bindir}/rustemd-tui

%changelog
* Sun Aug 23 2026 Francis Lalonde <6190260+fralalonde@users.noreply.github.com> - %{version}-1
- Initial release
