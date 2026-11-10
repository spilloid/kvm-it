# The workflow panel, section by section

Select a workflow in **Scripts…** and the panel below the list fills up with everything about it. That is a lot
of sections, but most of them you set up once and then leave closed. On a normal day you touch four things:
**This laptop**, the **sign-in and password**, **Run**, and the **Timeline**.

This page goes through the panel from top to bottom: what each part is for, when you need it, and when you can
ignore it. The job itself, screen by screen, is in [Provision Windows 11 laptops](provisioning.md).

## What you touch, and when

| When | What you use | Everything else |
|---|---|---|
| **Once per organisation** | **Set up your copy**, **Save my copy**, then a **Dry run** | leave closed |
| **Each laptop** | **This laptop**, sign-in, password, the review tick, **▶ Run**, **Timeline** | leave closed |
| **When a run stops** | the message, **Timeline**, **Resume from …** (or **Recover the interrupted run** first, if it is offered) | leave closed |
| **When a laptop shows a screen in other words** | **Screen wording** | leave closed |
| **After a run, for a ticket** | **Copy notes** or **Save trace and notes** | leave closed |
| **Rarely** | **Approved profile** (to change a setting), **Screens** (pictures, only where text cannot be read), **Variables** (troubleshooting) | |

**A greyed-out button always says why.** Hover over it: "Enter the password first.", "Open a video source first: a
dry run still watches the screen.", "Save profile (or Undo changes) first: a run uses the saved profile.", and so on.
Do what it says and the button comes on.

## From top to bottom

### The list and the folder

The scripts and workflows in your scripts folder, with the built-in ones marked **[built-in workflow]**. **Reload
folder** reads the folder again (after you copy a file in, or edit one in a text editor). The folder's path is shown
under the button.

### The workflow's name and states

The name and description, then **States**: every screen the workflow knows, in order (`REGION · KEYBOARD · …`).
These are the names the timeline, the messages and **Resume** use. Read only; nothing to do here.

### Set up your copy / Approved profile

Your organisation's settings: how laptops join (Autopilot or a work account), your organisation's short name, your
sign-in domain, and whether to accept the licence terms.

- On the **built-in** workflow it is called **Set up your copy** and opens by itself. Fill it in, check the **File
  name**, click **Save my copy**. The app writes your copy into the scripts folder and selects it. The built-in
  workflow itself never runs; you always run your copy.
- On **your copy** it is called **Approved profile** and stays closed. Open it only to change a setting, then
  click **Save profile** (or **Undo changes**). Saving changes the file, so a checkpoint saved before the change will
  not resume.

**Fixed in the file** lists values the form does not offer; they are part of the workflow, not settings.

### The sign-in and password

Fields for what the workflow types that must never be saved: here, the user's sign-in and password. They are held
in memory only, never saved, logged or exported, and they are cleared when you select another workflow. A **Dry run**
does not need them.

### Screens

How each screen is recognised. Where this computer can read text (Windows, macOS, or Linux with Tesseract), the
title says *Screens (N of N recognised by their text)*, with the same number twice, and there is nothing to do.

You need it only where text cannot be read. Then each screen is recognised from a saved picture instead: the list
shows each picture as **ok**, **missing** or **not needed**. To save one, bring the laptop to that screen, choose the
picture and click **Save this screen as**. A run that checks the sign-in does not start without a text reader.

### Screen wording

The phrases each screen is recognised by. You need it when a laptop shows a screen in words the workflow does not
know (another Windows build), which makes the run stop with *the screen was not recognised*.

1. Leave that screen on the video. Open **Screen wording** and choose the screen it should be.
2. Click **Read the screen now**. The panel lists the lines on screen and says whether they match this screen's
   wording.
3. Click the line that names the screen (usually its heading). It is added as a new wording.
4. Click **Save wording**. Only that screen's phrases change in your copy.

A wording with several phrases (joined by **and**) needs all of them on screen. Separate wordings are alternatives:
any one is enough. **+ phrase** adds a phrase to a wording, **x** removes one, **Add a wording** starts a new one.
The built-in workflow is shown but not changed here: set up your copy first.

### The review tick

*I wrote or reviewed this workflow — it will type into the target.* The tick is needed once per selection for any
workflow from your scripts folder, so that nothing types into a laptop that nobody has looked at.

### Recover the interrupted run (only when needed)

It appears in red only when a run did not end cleanly: the app was closed, it crashed, or the computer stopped while
a run was going. It says which step the run reached. Check the laptop, click **Recover the interrupted run**, then
**Resume**. Click it only when no run of this workflow is going on in another window or a terminal; the app refuses
it while one is.

### This laptop

The laptop's asset tag or serial. It is optional, but it names the run's record and its checkpoint: **Resume** carries
on only on the laptop it names, so moving the adapter to the next laptop never resumes the last one's run there.
It cannot contain the password or sign-in.

### The run buttons

- **▶ Run** types into the laptop. It needs the adapter connected, the sign-in and password, the review tick, and the
  profile saved.
- **Dry run** watches the screen and sends nothing. It needs only the video. Use it once per copy, clicking through
  setup by hand, to check that the timeline names each screen.
- **Resume from STATE (time)** appears when a stopped run left a checkpoint. It looks at the screen first and carries
  on only if the laptop shows that screen or one that may follow it. If keys may have been sent already, it asks
  before sending them again.
- **■ Abort** (or **Esc**) stops a run. Nothing is left held down.

### State and Timeline

**State** shows where the run is now, coloured like the chips: amber while it runs (with *your turn* when a person
must act, and the attempt number when it tries a step again), green **Done**, red **Stopped in …**. The **Timeline** lists every screen recognised, every step done and every check, with the
time since the start.

**Your turn** means a person must do this step at the laptop: connect the network, approve the sign-in on a phone,
choose privacy settings. Do it there; the run sees the screen move on and continues by itself.

### Variables

What the run knows, and where each value came from: the profile (*configured*), read off the screen
(*observed*), recorded when a step was checked (*verified*), or the run's own bookkeeping (*runtime*). Only for troubleshooting.
Prompted secrets are never listed.

### Save trace and notes, Copy notes

These appear after a run ends. **Copy notes** puts a Markdown summary on the clipboard for a ticket. **Save trace and
notes** writes the full trace and the notes into the scripts folder. Neither contains the password or anything typed.
Every live run already saves its record in `runs\` by itself; these are for when you want a copy now.

### The run log (bottom of the window)

While any script or workflow runs, a strip along the bottom of the window shows its log and its own **■ Abort**.
Drag its top edge to make it taller.

## Words the panel uses

| Word | Means |
|---|---|
| **workflow** | a file that drives a job screen by screen: it recognises each screen and types only on the one it expects |
| **your copy** | your organisation's own workflow file, made from the built-in one by **Save my copy** |
| **profile** | your copy's settings (method, organisation, domain, licence) |
| **state** | one screen the workflow knows, such as `REGION` or `PASSWORD` |
| **wording**, **phrase** | the text a screen is recognised by; a wording is one or more phrases that must all be on screen |
| **dry run** | a run that only watches and sends nothing |
| **Your turn** | a step a person does at the laptop; the run waits and carries on when the screen moves on |
| **checkpoint** | where a stopped run is kept, so **Resume** can carry on; one per workflow and adapter |
| **run record** | the trace and notes every live run saves in `runs\`; no secrets |
| **trace**, **notes** | the full record of a run (JSONL) and its readable summary (Markdown) |
