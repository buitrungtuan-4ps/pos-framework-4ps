---
kind: added
upgrade_note: `GET /api/package` gains `running.screens`, omitted when the version has none; a till on an older release ignores it and keeps drawing its default screens.
---
- **A till draws the screens of the package version its store runs.** A screen the version
  declares replaces the release's screen of the same id, in the layout for the till's device
  class; a button on it that opens another screen works. A store on the default package sees no
  change.
