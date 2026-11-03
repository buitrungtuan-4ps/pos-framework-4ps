---
kind: changed
---
- **Splitting a bill and putting it back together wait for the store server.** The till refuses
  parts that do not cover the bill's lines exactly once, and a merge of bills it knows are on
  different orders, before sending anything. It shows the act as pending until the store server
  answers, shows a refusal in place for the cashier to dismiss, and puts the first part on screen
  only once the split is made. If no answer comes, the till never sends the act again: it reads
  the table's bills until it can tell whether the split or merge was made.
