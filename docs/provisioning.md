# Provision Windows 11 laptops

kvm-it can take a new Windows 11 laptop from the first setup screen to the user's first sign-in, typing only on the
screens it recognises and stopping (typing nothing) on anything else. You set it up once for your organisation, save a
picture of each setup screen once, and then run it on each laptop: you plug in the network, approve MFA and choose the
privacy settings when it asks; it does the rest.

It works two ways:

- **Windows Autopilot**: the laptop is registered for Autopilot and shows your organisation's sign-in right after the
  network.
- **Work or school account**: the laptop is not registered; setup is told it is for work or school and joins Microsoft
  Entra with the user's account.

> **How far it has been tested.** In a Windows 11 25H2 virtual machine, the work-account path ran by itself from the
> region screen to the work-or-school sign-in page, through updates and a restart. Typing the user name and password,
> the organisation check, MFA, enrolment, the Autopilot path, and real laptops have not been run yet. Watch your first
> laptop all the way through, and save a trace if anything stops (see [When it stops](#when-it-stops)).

## What you need

- kvm-it 0.5.1 or later, with an adapter and a capture card ([Get started](getting-started.md)).
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
| If setup shows the Windows licence terms | **Accept them**, or **Stop there** for a person to accept |

Click **Save my copy**. It writes `windows-11-oobe-provisioning-<name>.toml` into your scripts folder (shown under the
list) and selects it.

**From a terminal:**

```
kvmit workflow new windows-11-oobe-provisioning
```

It asks for each setting (Enter keeps the value shown). To answer up front instead:

```
kvmit workflow new windows-11-oobe-provisioning --set deployment_mode=work_account --set tenant=fabrikam --set accept_license=yes
```

`kvmit workflow show <name or file>` lists the settings and their choices. To change one later, use the copy's
**Approved profile** in the app (**Save profile**), or `kvmit workflow set <file> --set name=value`. Only these
settings change; the screens and keys stay as they are. A checkpoint saved before the change will not resume.

If you provision for more than one organisation, or with both methods, make a copy for each.

## 2. Save the setup screens (once)

The workflow recognises each screen from a picture of it, so it needs one picture of each screen from your own laptops
(the capture card and the laptop's resolution change how a screen looks). With your copy selected, open
**Reference screens**: each picture is listed, marked *missing* until it is saved.

Take one laptop through setup **by hand**, from the keyboard you are driving through kvm-it. At each screen, choose that
screen's picture under **Save this screen as** and click it:

| Picture | Screen | Who does it |
|---|---|---|
| `region.png` | Is this the right country or region? | the workflow (accepts the one selected) |
| `keyboard.png` | Is this the right keyboard layout? | the workflow (accepts the one selected) |
| `second-keyboard.png` | Want to add a second keyboard layout? | the workflow (Skip) |
| `network.png` | Let's connect you to a network | **you**: plug in Ethernet or join Wi-Fi, then Next |
| `updates.png` | Checking for updates / Just a moment | nobody: it waits (up to 45 min, restarts included) |
| `license.png` | License agreement | the workflow, if your copy accepts it |
| `device-name.png` | Name your device | the workflow (Skip; Intune or Autopilot naming can set it) |
| `setup-choice.png` | How would you like to set up this device? | the workflow (work or school; work-account copies only) |
| `autopilot-signin.png` | Welcome to your organisation: sign in | the workflow types the user name (Autopilot copies only) |
| `work-signin.png` | Let's set things up for your work or school | the workflow types the user name (work-account copies only) |
| `password.png` | Enter password | the workflow types the password, **after** checking your branding |
| `tenant-<name>.png` | the same Enter password page, with your organisation's branding | the organisation check |
| `mfa.png` | Approve sign-in request / enter code | **the user**: approves on their phone (up to 10 min) |
| `privacy.png` | Choose privacy settings for your device | **you**: choose your organisation's settings, then Accept |
| `esp.png` | Setting up your device for work | nobody: it waits for enrolment (up to 90 min) |
| `hello.png`, `desktop.png` | Windows Hello / PIN setup, or the desktop | the end: the user takes over |

Notes:

- Save every screen you can reach. Some you may not be able to: the licence terms (often not shown), the network
  screen (skipped when Ethernet is plugged in; unplug it for the walkthrough to save it), the other method's sign-in
  page (`autopilot-signin.png`, or `setup-choice.png` and `work-signin.png`), MFA and enrolment if your organisation has
  none. To run without them, tick **Run without the missing pictures** under Reference screens (or add
  `--allow-missing-pictures`). A screen without a picture is not recognised: if it appears, the run usually stops there
  as an unknown screen, but it **may be taken for a similar screen** and that screen's keys typed. The likeliest case is
  a laptop on the other method showing its sign-in page, which could be taken for your method's sign-in page and get the
  user name. The password is still typed only after your organisation's branding matches. A dry run never needs the
  tick.
- Your organisation's picture (`tenant-<name>.png`) is required, and so is the picture of every other organisation
  listed under `[tenants]` in your copy: a run will not start without them.
- Save `password.png` and `tenant-<name>.png` from the same page, once your organisation's branding (logo, background,
  sign-in text) has loaded.
- From a terminal, `kvmit run <your copy>.toml --check` lists the same pictures, present or missing, and
  `kvmit video snap <path>` saves the current screen as one.

## 3. Dry run (once)

Reset a laptop to the first setup screen (or use the next new one), select your copy and click **Dry run**. It watches
the screen and sends nothing, so it needs no sign-in or password. Click through setup by hand and check that the timeline names each screen as you reach
it. If a screen is not recognised, save its picture again from that laptop.

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
| *the screen was not recognised with confidence (closest: X at N%)* | the screen does not match any saved picture well enough | finish that screen by hand, then save its picture from this laptop for next time |
| *expected X but the screen shows Y* | setup went somewhere the workflow does not allow from here | check the laptop; complete the step by hand, then Resume |
| *entry check failed: deployment_mode* | the laptop showed the other method's screen (Autopilot vs work account) | the laptop is (or is not) registered for Autopilot: use the other copy, or fix the registration |
| *entry check failed: accept_license* | the licence screen appeared and your copy says stop there | accept it by hand, then Resume |
| *does not show tenant X's branding; stopped before enrolling* | the password page is not your organisation's (wrong account, or branding still loading) | check the sign-in name; if it is right, save `tenant-<name>.png` again from this page |
| *the screen did not move on* | the step was done but the next screen never came (slow network, a dialog) | finish by hand, then Resume |
| *does not match the checkpoint; not resuming* | the laptop is no longer where the run stopped | run again from the start (it recognises the screen it is on) |
| *another run of this workflow on this adapter is in progress* | a run of this copy is already using this adapter, or one crashed | if none is running, check the laptop, delete the `.lock` file named in the message, then run again |

If something stops that this table does not explain, save the trace and notes and send them with the ticket.

## Where the files are

Everything lives in your scripts folder (`Documents\kvm-it\scripts` on Windows; shown under the Scripts list):

| Path | What |
|---|---|
| `windows-11-oobe-provisioning-<name>.toml` | your copy |
| `oobe-win11\*.png` | the saved screens (shared by every copy in the folder) |
| `checkpoints\` | where a stopped run is kept for Resume (no secrets, no pictures) |
| `workflow-run-*.trace.jsonl`, `*.notes.md` | saved traces and notes |

How workflows work in general (states, checks, writing your own) is in [the UX notes](ux.md#stateful-workflows-issue-46).
