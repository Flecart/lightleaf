# Security policy

Please **do not open a public issue** for security vulnerabilities.

Report them privately through GitHub's
[security advisory form](https://github.com/Flecart/lightleaf/security/advisories/new). Include the steps to
reproduce and the affected version or commit. You should get a reply within a few days.

## Scope notes

Lightleaf runs TeX and Typst on user-supplied documents. The built-in restrictions are described in the README's
"Security notes". In particular, LuaLaTeX can read files outside the project, so deployments open to untrusted users
should sandbox compilers with `LIGHTLEAF_COMPILE_WRAPPER` or the provided Docker setup. Reports that only rely on this
documented limitation are still welcome, but are treated as hardening rather than vulnerabilities.
