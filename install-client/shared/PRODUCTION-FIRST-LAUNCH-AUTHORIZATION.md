# First-launch authorization

Formal WPTSALL releases use minisign and SHA-256 for artifact integrity. The
public release verification key is:

`RWRK0RTOL1wb3YAJ6sywCLsN0kvpOttJ1CYv3AHluZAfYUXzSEm/0cSw`

```sh
minisign -Vm kit-webui-linux-x86_64.tar.gz -P RWRK0RTOL1wb3YAJ6sywCLsN0kvpOttJ1CYv3AHluZAfYUXzSEm/0cSw
```

Use the matching file and `.minisig` for your product and platform. Native
packages and portable ZIPs have their own signatures. Unsigned Actions
artifacts and temporary test-key prereleases are not official releases.

Minisign does not replace Apple Developer ID/notarization or Microsoft
Authenticode. These builds do not claim those OS certificates. macOS and
Windows may warn about an unknown publisher. Verify the formal artifact and
its signature before granting first-launch permission through OS settings.
Do not disable system-wide security protections.

Both products store settings and task data outside the package directory.
Native uninstall preserves that per-user data. Back it up before updates.
