# Provision Windows 11 laptops

kvm-it can take a new Windows 11 laptop from the first setup screen to the user's first sign-in, typing only on the
screens it recognises and stopping (typing nothing) on anything else. It recognises each screen by **reading its text**
("Is this the right country or region?"), so there is nothing to save first: you set it up once for your
organisation, then run it on each laptop. You plug in the network, approve MFA and choose the privacy settings when it
asks; it does the rest.

It works two ways:

- **Windows Autopilot**: the laptop is registered for Autopilot and shows your organisation's sign-in right after the
  network.
- **Work or school account**: the laptop is not registered; setup is told it is for work or school and joins Microsoft
  Entra with the user's account.

> **How far it has been tested (0.6.0).** In a Windows 11 25H2 virtual machine, the work-account path ran by itself
> from the region screen to the work-or-school sign-in page, recognising every screen by its text alone, through
> updates and a restart; a typed sign-in was read back in the field, and with Caps Lock on the run stopped before
> Enter. The password, the organisation check on a real password page, MFA, enrolment, the Autopilot path and real
> laptops have not been run yet, and the text of the screens after the sign-in is Microsoft's usual wording, not yet
> seen by kvm-it. Watch your first laptop all the way through, and save a trace if anything stops
> (see [When it stops](#when-it-stops)).

## What you need

- kvm-it 0.6.0 or later, with an adapter and a capture card ([Get started](getting-started.md)).
- A computer that reads text off the screen: **Windows** (built in; English must be among its languages, as it is on
  English Windows), **macOS** (built in), or **Linux** with Tesseract installed (`sudo apt install tesseract-ocr`).
  Without it, screens can still be recognised from saved pictures, but a run that checks the sign-in does not start.
- **One adapter and one capture card per laptop** you want to run at the same time.
- The user's sign-in name and password for each laptop. They are asked for at the start of each run, held in memory, and
  never saved.

## 1. Set up your copy (once)

The built-in workflow names an example organisation. Make your own copy:

**In the app:** open **Scripts…**, select **[built-in workflow] windows-11-oobe-provisioning.toml**, and fill in
**Set up your copy**:

| Setting | Choose |
|---|---|
| How the device joins your organisation | **Windows Autopilot** or **Work or school account** |
| Your organisation's short name | a short name such as `fabrikam` (letters, digits, `-`, `_`) |
| Your organisation's sign-in domain | the part after `@` in your users' sign-ins, such as `fabrikam.com` |
| If setup shows the Windows licence terms | **Accept them**, or **Stop there** for a person to accept |

The method and the licence have no default: choose both (**Save my copy** stays off until you do, and the built-in
workflow itself does not run). Click **Save my copy**. It writes `windows-11-oobe-provisioning-<name>.toml` into your scripts folder (shown under the
list) and selects it.

**From a terminal:**

```
kvmit workflow new windows-11-oobe-provisioning
```

It asks for each setting (Enter keeps the value shown; the method and the licence must be chosen). To answer up front
instead:

```
kvmit workflow new windows-11-oobe-provisioning --set deployment_mode=work_account --set tenant=fabrikam --set tenant_domain=fabrikam.com --set accept_license=yes
```

`kvmit workflow show <name or file>` lists the settings and their choices. To change one later, use the copy's
**Approved profile** in the app (**Save profile**), or `kvmit workflow set <file> --set name=value`. Only these
settings change; the screens and keys stay as they are. A checkpoint saved before the change will not resume.

If you provision for more than one organisation, or with both methods, make a copy for each.

## 2. The screens it knows

Each screen is recognised by a phrase it shows, read in the part of the screen where setup puts its heading:

| Screen | Recognised by | Who does it |
|---|---|---|
| Country or region | *Is this the right country or region?* | the workflow (accepts the one selected) |
| Keyboard | *Is this the right keyboard layout or input method?* | the workflow (accepts the one selected) |
| Second keyboard | *Want to add a second keyboard layout?* | the workflow (Skip) |
| Network | *Let's connect you to a network* | **you**: plug in Ethernet or join Wi-Fi, then Next |
| Updates | *Checking for updates*, *Please keep your PC on* or *Just a moment* | nobody: it waits (up to 45 min, restarts included) |
| Licence | *License agreement* | the workflow, if your copy accepts it |
| Device name | *Name your device* and *Make this device yours* | the workflow (Skip; Intune or Autopilot naming can set it) |
| Setup choice | *How would you like to set up this device?* | the workflow (work or school; work-account copies only) |
| Autopilot sign-in | *Welcome to* and *Sign in* | the workflow types the sign-in (Autopilot copies only) |
| Work sign-in | *Let's set things up for your work or school* | the workflow types the sign-in (work-account copies only) |
| Password | *Enter password* | the workflow types the password, **after** checking the page shows that sign-in |
| MFA | *Approve sign in request*, *Enter code* or *Verify your identity* | **the user**: approves on their phone (up to 10 min) |
| Privacy | *Choose privacy settings for your device* | **you**: choose your organisation's settings, then Accept |
| Enrolment | *Setting up your device for work* | nobody: it waits for enrolment (up to 90 min) |
| The end | *Windows Hello*, *Set up a PIN*, *Create a PIN* or the desktop's *Recycle Bin* | the user takes over |

The first eight and the work sign-in were read off Windows 11 25H2; the rest are Microsoft's usual wording. If a laptop
shows a screen in other words, the run stops there as an unknown screen, typing nothing: finish that screen by hand and
Resume, and send the trace so the phrase can be added.

The sign-in is checked twice before the password is typed:

- **Before Enter on the sign-in page**, the field must show the sign-in exactly as typed. If it shows in capitals,
  Caps Lock is on at the laptop; if a symbol is wrong (a `"` where the `@` should be), the laptop uses another keyboard
  layout than US. Either way the run stops before Enter.
- **On the password page**, it must show that very account, in your sign-in domain. Otherwise the run stops before the
  password: it may be another organisation's page, or the wrong account.

**Pictures, if you need them.** Where text cannot be read, each screen can be recognised from a saved picture instead
(**Reference screens** in the app lists them, and **Save this screen as** saves the current video as one; or
`kvmit video snap <path>`). A run that checks the sign-in still does not start there. `kvmit run <your copy>.toml
--check` says how each screen will be recognised on this computer.

## 3. Dry run (once)

Reset a laptop to the first setup screen (or use the next new one), select your copy and click **Dry run**. It watches
the screen and sends nothing, so it needs no sign-in or password. Click through setup by hand and check that the
timeline names each screen as you reach it. A screen it does not name is worded differently on your laptops: note it
for the ticket.

## 4. Run it on each laptop

1. Plug the adapter into the laptop's USB port and its HDMI output into the capture card. Turn it on and wait for the
   region screen.
2. In kvm-it, select your copy. Enter the user's **sign-in** and **password**, tick that you reviewed the workflow, and
   click **Run** (or `kvmit run <your copy>.toml`).
3. Watch the timeline. At **Your turn** steps, do the step at the laptop: connect the network, have the user approve MFA,
   choose the privacy settings. The run sees the screen move on and continues by itself.
4. It ends at Windows Hello or the desktop: hand the laptop to the user.

Add `--save-trace` (or click **Save trace and notes** after the run) to keep a record for the ticket. Traces never contain
the password or anything typed.

### Several laptops at once

Each laptop needs its own adapter and capture card. In the terminal, run one `kvmit run` per laptop, each with its own
`--device <adapter>` and `--video <card>` (`kvmit scan` and `kvmit video list` show them). Each adapter keeps its own
checkpoint, so a stopped laptop resumes from its own last step, and the same workflow can run on many adapters at once.

## When it stops

A run stops before typing on a screen it does not recognise or does not allow. Keys it already sent stay sent: if it
stops because the screen did not move on after typing, check what the laptop shows. The app (and the terminal) says
why, and a stopped run saves a checkpoint. Fix the laptop, then click **Resume from …** (or `kvmit run <your copy>.toml --resume`): it looks at the screen
first and carries on only from the step it stopped at or the next one.

| It says | What happened | What to do |
|---|---|---|
| *the screen was not recognised with confidence (closest: X at N%)* | the screen's text matches no screen the workflow knows | finish that screen by hand, then Resume; send the trace so its wording can be added |
| *what was typed shows with upper and lower case swapped: Caps Lock looks on* | Caps Lock is on at the laptop | turn it off at the laptop, clear the sign-in field, then Resume |
| *what was typed does not show on screen as typed* | the laptop uses another keyboard layout than US, or the field did not take the text | set the layout to US at the laptop (or check the field), clear it, then Resume |
| *the page does not show the sign-in entered (an account in X)* | the password page is for another account or organisation, or still loading | check the sign-in; if it is right and the page shows it, Resume |
| *the sign-in entered is not an account in X* | the sign-in typed at the start is not in your copy's sign-in domain | check the sign-in, or your copy's domain |
| *this workflow checks the sign-in by reading the screen, and text cannot be read here* | this computer has no text reader (see [What you need](#what-you-need)) | install one (Linux: Tesseract), or use another computer |
| *expected X but the screen shows Y* | setup went somewhere the workflow does not allow from here | check the laptop; complete the step by hand, then Resume |
| *entry check failed: deployment_mode* | the laptop showed the other method's screen (Autopilot vs work account) | the laptop is (or is not) registered for Autopilot: use the other copy, or fix the registration |
| *entry check failed: accept_license* | the licence screen appeared and your copy says stop there | accept it by hand, then Resume |
| *the screen did not move on* | the step was done but the next screen never came (slow network, a dialog) | finish by hand, then Resume |
| *does not match the checkpoint; not resuming* | the laptop is no longer where the run stopped | run again from the start (it recognises the screen it is on) |
| *another run of this workflow on this adapter is in progress* | a run of this copy is already using this adapter, or one crashed | if none is running, check the laptop, delete the `.lock` file named in the message, then run again |

If something stops that this table does not explain, save the trace and notes and send them with the ticket.

## Where the files are

Everything lives in your scripts folder (`Documents\kvm-it\scripts` on Windows; shown under the Scripts list):

| Path | What |
|---|---|
| `windows-11-oobe-provisioning-<name>.toml` | your copy |
| `oobe-win11\*.png` | saved screen pictures, only where text cannot be read (shared by every copy in the folder) |
| `checkpoints\` | where a stopped run is kept for Resume (no secrets, no pictures) |
| `workflow-run-*.trace.jsonl`, `*.notes.md` | saved traces and notes |

How workflows work in general (states, checks, writing your own) is in [the UX notes](ux.md#stateful-workflows-issue-46).
