# ST8WRX release status

ST8WRX does not currently publish production artifacts from GitHub Actions.

The inherited Block/Buzz desktop, mobile, relay, Helm, Sprig, GHCR, candidate,
promotion, and canary publication workflows were removed during repository
independence work. Only validation workflows remain under `.github/workflows/`.

Before introducing any ST8WRX release workflow:

1. define the artifact and owner;
2. use ST8WRX-controlled package/image destinations;
3. require an explicit tag or manual approval;
4. grant the minimum GitHub token permissions;
5. prevent fork and pull-request publication;
6. document rollback and signing; and
7. test the workflow without publishing.

Do not restore upstream Block organization names, credentials, package paths,
release apps, or GHCR destinations.
