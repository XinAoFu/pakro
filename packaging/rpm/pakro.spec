Summary: Dev tools, env vars, and tasks in one CLI
Name: pakro
Version: 2026.10.3
Release: 1
Group: System
License: MIT
BuildRoot: /root/pakro

%description
pakro prepares your development environment before each command runs.

%install
mkdir -p %{buildroot}/usr/bin/
cp /root/pakro/target/release/pakro %{buildroot}/usr/bin
cp /root/pakro/cli/man/man1/pakro.1 %{buildroot}/%{_mandir}/man1

%files
/usr/bin/pakro
%{_mandir}/man1/pakro.1
