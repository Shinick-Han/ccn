# CCN plugin for TTT

The plugin now lives in its own repository:

**https://github.com/Shinick-Han/ttt-ccn**

Install it from TTT's **Plugins** panel (once registered), or with
**Plugins: Install from URL** using the URL above.

```powershell
# manual user install (Windows)
$dest = "$HOME\.config\ttt\plugins\ttt-ccn"
New-Item -ItemType Directory $dest -Force | Out-Null
git clone https://github.com/Shinick-Han/ttt-ccn $dest
```

See the plugin README for requirements, keys, permissions, demo and registry
notes. The CCN binary remains here; the TTT integration is a separate product.
