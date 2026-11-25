---
kind: fixed
---
- **The data designer's fields, lists, list entries and object types can be checked and published.**
  The designer gave each one an id made from the name it was added under, such as `occasions`,
  while the cloud reads every one of them as a ULID. So **Check** on any draft with custom data
  stopped at the request itself, with the raw message "Failed to deserialize the JSON body … a ULID
  is 26 characters", and nothing with custom data could be published from the console.
  - Each is now given a new ULID when it is added. The name it was added under becomes its label
    key, as before (`field.<name>`, `list.<name>`, `list.<name>.<entry>`, `object.<name>`), and
    the designer shows the label key where it showed the id, and offers a list by its label key.
  - Nothing that was published changes: no version with custom data could have been published from
    the console before this.
