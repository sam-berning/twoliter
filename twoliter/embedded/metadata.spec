%global cross_generate_attribution %{nil}

Name: %{_cross_os}metadata
Version: 1.0
Release: 1%{?dist}
Summary: Bottlerocket metadata

License: Apache-2.0 OR MIT
URL: https://github.com/bottlerocket-os/twoliter

Provides: %{_cross_os}variant(%{_cross_variant})
Provides: %{_cross_os}variant-platform(%{_cross_variant_platform})
Provides: %{_cross_os}variant-runtime(%{_cross_variant_runtime})
Provides: %{_cross_os}variant-family(%{_cross_variant_family})
Provides: %{_cross_os}variant-flavor(%{_cross_variant_flavor})

%if %{with grub_set_private_var}
Provides: %{_cross_os}image-feature(grub-set-private-var)
%else
Provides: %{_cross_os}image-feature(no-grub-set-private-var)
%endif

%if %{with uefi_secure_boot}
Provides: %{_cross_os}image-feature(uefi-secure-boot)
%else
Provides: %{_cross_os}image-feature(no-uefi-secure-boot)
%endif

%if %{with systemd_networkd}
Provides: %{_cross_os}image-feature(systemd-networkd)
%else
Provides: %{_cross_os}image-feature(no-systemd-networkd)
%endif

%if %{with unified_cgroup_hierarchy}
Provides: %{_cross_os}image-feature(unified-cgroup-hierarchy)
%else
Provides: %{_cross_os}image-feature(no-unified-cgroup-hierarchy)
%endif

%if %{with xfs_data_partition}
Provides: %{_cross_os}image-feature(xfs-data-partition)
%else
Provides: %{_cross_os}image-feature(no-xfs-data-partition)
%endif

%if %{with fips}
Provides: %{_cross_os}image-feature(fips)
%else
Provides: %{_cross_os}image-feature(no-fips)
%endif

%description
%{summary}.

%prep

%build

%install

%files

%changelog
