# C1 System Context

`bak` is a cross-platform file backup management software for individual users, providing functions such as file backup and restore.

After startup, `bak` provides an HTTP API. Individual users and automation scripts operate it through HTTP clients such as `curl`, authenticating with a Bearer secret key.

`bak` reads regular file contents and directory structure from source file system, stores backup contents in a local repository, and stores manifest information required for identification, verification, and restore.

Users / Automation Scripts <---> bak <---> file system