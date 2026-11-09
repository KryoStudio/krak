# Signing and scanning releases

Krak's installer is not Authenticode-signed yet. Windows therefore shows SmartScreen's "Windows
protected your PC" screen to whoever downloads it, and Microsoft Defender sometimes flags a new
version on sight. This page explains what the release workflow
([`.github/workflows/release.yml`](../.github/workflows/release.yml)) can do about it, and how to
switch each part on. Nothing here is needed to build Krak yourself.

The in-app updates are a separate matter: they are signed with the minisign key of
`TAURI_SIGNING_PRIVATE_KEY` (see the README) and checked by the app against the public key in
`tauri.conf.json`, signed installer or not.

## What the workflow does

| Configured                         | What a release gets                                                                                  |
| ---------------------------------- | ---------------------------------------------------------------------------------------------------- |
| Nothing (today)                    | Built by `tauri-action`, unsigned. The release notes get the installer's SHA-256.                     |
| `VT_API_KEY` secret                | The same, plus a VirusTotal scan of the installer, linked from the release notes with its verdict.  |
| `SIGNPATH_ORGANIZATION_ID` variable | `krak.exe` and the installer are Authenticode-signed by SignPath before being published.            |

The two options are independent. While `SIGNPATH_ORGANIZATION_ID` is unset, the build goes through
`tauri-action` exactly as before.

With SignPath on, a release goes:

1. `tauri build --no-bundle` builds `krak.exe`, which is uploaded as a workflow artifact and sent to
   SignPath (first signing request).
2. The signed `krak.exe` replaces the unsigned one and `tauri bundle --bundles nsis` makes the
   installer around it.
3. The installer goes to SignPath (second signing request) and comes back signed.
4. The updater's signature (`.sig`) made while bundling covered the unsigned installer, so the
   in-app updater would reject the signed one: `tauri signer sign` signs the final installer again
   and `latest.json` is written for it.
5. Both Authenticode signatures are checked, and the draft release is created with the installer,
   its `.sig` and `latest.json`.

Each signing request waits for its approval in SignPath (three hours at most; the job fails
otherwise and can be re-run).

## Code signing with SignPath Foundation

[SignPath Foundation](https://signpath.org) signs open-source software for free, with a certificate
issued to the foundation. Read [its terms](https://signpath.org/terms) before applying; in short:

- The project uses an OSI-approved license (Krak: GPL-3.0-only) for all its code, without
  proprietary parts, is maintained, and has already been released in the form that gets signed.
  Krak loads NVIDIA's NVENC library from the driver at run time and embeds Microsoft's WebView2
  bootstrapper: say so in the application, they are system components and not shipped as Krak code.
- The project's home page (krakclip.com, or the README) has a section or link titled
  **"Code signing policy"**, which states: *Free code signing provided by
  [SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org)*,
  lists the team's roles (authors, reviewers, approvers: for a one-person project, the owner holds
  all three), and links a privacy policy or states what the program sends over the network. For
  Krak: update checks against GitHub, Discord Rich Presence through the local Discord app, and the
  optional "Share to the Krak community" upload, only when the user asks for it.
- Everyone on the team uses multi-factor authentication on GitHub and on SignPath.
- Releases are built from the repository by GitHub Actions on GitHub-hosted runners (as here), and
  every release is approved by hand in SignPath before it is signed.
- The signed files carry the product name and version (Tauri sets both from `tauri.conf.json`).
- Signed software must not be malware, a hacking tool, or change the system without telling the
  user, and the installer needs an uninstaller (Krak's NSIS installer has one).

The certificate is issued to SignPath Foundation, so Windows shows **"SignPath Foundation"** as the
publisher, not Kryo Studio. That is expected, and it is what makes the program free; no company is
needed.

### Applying

1. Turn on two-factor authentication on GitHub if it is not on yet.
2. Add the "Code signing policy" section described above to the website or the README.
3. Apply from [signpath.org](https://signpath.org) ("Apply"), with the repository's URL. SignPath
   reviews the project and then creates an organization for it on [app.signpath.io](https://app.signpath.io).

### Setting up SignPath

In the SignPath organization:

1. Add the **GitHub.com trusted build system** and link it to the project (SignPath then only
   accepts artifacts built by GitHub Actions from this repository). Install the SignPath GitHub
   App if asked.
2. Create (or ask the foundation for) a project with the slug `krak` and a signing policy with the
   slug `release-signing` (other slugs work: see the variables below).
3. Create two artifact configurations. The workflow uploads each file inside a ZIP (what
   `actions/upload-artifact` makes):

   `krak-exe`:

   ```xml
   <?xml version="1.0" encoding="utf-8"?>
   <artifact-configuration xmlns="http://signpath.io/artifact-configuration/v1">
     <zip-file>
       <pe-file path="krak.exe">
         <authenticode-sign />
       </pe-file>
     </zip-file>
   </artifact-configuration>
   ```

   `krak-installer` (the workflow always names it `krak-setup.exe` for signing, then puts it back
   under its real name, `Krak_<version>_x64-setup.exe`):

   ```xml
   <?xml version="1.0" encoding="utf-8"?>
   <artifact-configuration xmlns="http://signpath.io/artifact-configuration/v1">
     <zip-file>
       <pe-file path="krak-setup.exe">
         <authenticode-sign />
       </pe-file>
     </zip-file>
   </artifact-configuration>
   ```

4. Create a CI user with the **submitter** role on the signing policy, and an API token for it.

### Turning it on in GitHub

In the repository: **Settings → Secrets and variables → Actions**.

| Name                               | Kind     | Value                                                     |
| ---------------------------------- | -------- | --------------------------------------------------------- |
| `SIGNPATH_API_TOKEN`               | Secret   | The CI user's API token.                                  |
| `SIGNPATH_ORGANIZATION_ID`         | Variable | The organization's ID (a GUID). **Setting it turns signing on.** |
| `SIGNPATH_PROJECT_SLUG`            | Variable | Optional, `krak` by default.                              |
| `SIGNPATH_POLICY_SLUG`             | Variable | Optional, `release-signing` by default (`test-signing` to try the setup). |
| `SIGNPATH_EXE_CONFIGURATION`       | Variable | Optional, `krak-exe` by default.                          |
| `SIGNPATH_INSTALLER_CONFIGURATION` | Variable | Optional, `krak-installer` by default.                    |

Then push a tag as usual. To go back to unsigned releases, delete `SIGNPATH_ORGANIZATION_ID`.

A signature does not remove SmartScreen's warning at once: SmartScreen also weighs the reputation
of the file and of its certificate, which grows as people download and run it. It helps that every
release is signed with the same certificate.

## When Defender flags a release

New, unsigned (or newly signed) programs that capture the screen and register global shortcuts
are sometimes taken for something they are not. If Defender (or SmartScreen) flags a Krak release:

1. Check the release's VirusTotal report (linked in its notes) to see which engines flag it.
2. Submit the installer to Microsoft as a false positive at
   <https://www.microsoft.com/en-us/wdsi/filesubmission>: choose **Software developer**, sign in
   with a Microsoft account, pick **Microsoft Defender Antivirus** (or **SmartScreen** for the
   download warning), mark it as **Incorrectly detected as malware/malicious**, and give the
   detection name, the download URL (the GitHub release asset) and a short explanation: an
   open-source game clipper, built by GitHub Actions from this repository, with the SHA-256 from
   the release notes. Microsoft usually answers within a few days, and the definitions update
   follows.
3. Other vendors that flag it on VirusTotal have their own false-positive forms; the most useful
   are listed in [VirusTotal's documentation](https://docs.virustotal.com/docs/false-positive-contacts).

## VirusTotal scans

1. Create a free account at [virustotal.com](https://www.virustotal.com) and copy the API key from
   your profile (**API key**).
2. Add it to the repository as the **secret** `VT_API_KEY`.

Each release then uploads its installer to VirusTotal (the free API: 4 requests a minute, which the
workflow respects), waits up to ten minutes for the result, and adds to the draft release's notes:

```markdown
**SHA-256** of `Krak_0.7.0_x64-setup.exe`: `…`

**VirusTotal:** [scan report](https://www.virustotal.com/gui/file/…) (0 of 70 engines flag it)
```

The key is only given to the scan step, never to the build. If VirusTotal is down or slow, the
release goes on without the scan, or with the link and "analysis still running". Files uploaded to
VirusTotal are shared with its partners: only the public installer is sent.
