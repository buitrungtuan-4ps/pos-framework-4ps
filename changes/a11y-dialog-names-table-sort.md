---
kind: fixed
---
- **Screen readers hear what a dashboard dialog is, which way a table is sorted, and where the pager
  is.** A `Modal` or `Drawer` is now named by its title (`aria-labelledby`), so opening one
  announces "Edit store, dialog" rather than "dialog". The column a table is sorted by carries
  `aria-sort` (`ascending` or `descending`); the caret beside it was hidden from assistive
  technology and said nothing. The pager is a `<nav>` landmark named "Pagination" ("Phân trang"),
  from the new translation key `table.pagination`. (#690, #691)
