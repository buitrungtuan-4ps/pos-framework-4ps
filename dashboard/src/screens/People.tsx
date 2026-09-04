// People & access (ADR-0070, Track M1), on the F2 CRUD kit. The operator's place to onboard a store's
// employees, set their sign-in PIN, author role templates over the pos-core permission catalogue, and
// assign people to a store with a role — all by name, no ULID typed. Tenant-scoped to the picker's
// context; assignments also need a store chosen in the top bar.
//
// This is the console's first T1 Restricted data (employee name, code, PIN). The PIN is set/reset,
// never read: it is hashed server-side and this screen only ever learns whether one is set. Write
// affordances are gated on the operator holding console.people.manage (owner/admin) — the server
// re-checks every route; the gate here only hides what a role cannot do.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { Assignment, Employee, Page, PermissionInfo, RoleTemplate } from "../api/types";
import { t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { actingAdmin, storeId, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";
import {
  type Column,
  ConfirmDialog,
  DataTable,
  Drawer,
  EmptyState,
  FormField,
  Modal,
  StatusBadge,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";

export function People() {
  // One page of the roster, not the whole thing (ADR-0098, #299). The table below is the only reader
  // now: an assignment row carries the person it names, and the assign picker searches the server.
  const [roster, setRoster] = createSignal<Page<Employee> | null>(null);
  const [rosterOffset, setRosterOffset] = createSignal(0);
  const [rosterQuery, setRosterQuery] = createSignal("");
  const [rosterSort, setRosterSort] = createSignal("newest");
  const [rosterDescending, setRosterDescending] = createSignal(false);
  let rosterQueryTimer: ReturnType<typeof setTimeout> | undefined;
  let rosterSeq = 0;
  const [roles, setRoles] = createSignal<RoleTemplate[]>([]);
  const [catalogue, setCatalogue] = createSignal<PermissionInfo[]>([]);
  const [assignments, setAssignments] = createSignal<Assignment[]>([]);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  // console.people.manage → owner/admin (mirrors the backend role set; the server re-checks).
  const canManage = () => {
    const role = actingAdmin()?.role;
    return role === "owner" || role === "admin";
  };

  // New employee.
  const [newCode, setNewCode] = createSignal("");
  const [newName, setNewName] = createSignal("");

  // PIN reset (a Modal over one employee).
  const [pinFor, setPinFor] = createSignal<Employee | null>(null);
  const [pinValue, setPinValue] = createSignal("");

  // Archive/restore.
  const [pendingArchive, setPendingArchive] = createSignal<Employee | null>(null);

  // Role editor (a Drawer). `roleDraftId` is "" for a new role, else the role being edited.
  const [roleOpen, setRoleOpen] = createSignal(false);
  const [roleDraftId, setRoleDraftId] = createSignal("");
  // The version the role drawer opened on (ADR-0094). Empty for a new role, which has none yet.
  const [roleDraftEtag, setRoleDraftEtag] = createSignal("");
  const [roleName, setRoleName] = createSignal("");
  const [rolePermissions, setRolePermissions] = createSignal<string[]>([]);

  // Assign form (store from the top-bar context).
  const [assignEmployee, setAssignEmployee] = createSignal("");
  const [assignRole, setAssignRole] = createSignal("");
  // The assign picker searches the roster on the server rather than filtering a loaded copy of it
  // (ADR-0098, B3-4). `assignChoice` holds the whole chosen row, not just the id, so the picker can
  // name who is selected without going back to a list it no longer keeps.
  const [assignSearch, setAssignSearch] = createSignal("");
  const [assignMatches, setAssignMatches] = createSignal<readonly Employee[]>([]);
  const [assignChoice, setAssignChoice] = createSignal<Employee | null>(null);
  let assignSearchTimer: ReturnType<typeof setTimeout> | undefined;
  let assignSearchSeq = 0;
  const [pendingRemove, setPendingRemove] = createSignal<Assignment | null>(null);

  // A `412` means somebody else saved this person or role while the form was open (ADR-0094). The
  // screen reloads rather than offering a retry: retrying would re-apply the overwrite the refusal
  // exists to prevent, and the operator needs to see what actually changed before deciding again.
  const fail = async (caught: unknown) => {
    if (caught instanceof ApiError && caught.isStale) {
      const message = t("people.stale");
      setError(message);
      toast.error(message);
      await load();
      return;
    }
    const message = caught instanceof ApiError ? caught.message : String(caught);
    setError(message);
    toast.error(message);
  };

  /** How many employees the table shows at once. The pager reads `total` for the rest. */
  const ROSTER_PAGE_SIZE = 12;

  /**
   * Reads one page of the roster under the current offset, search and order.
   *
   * Sequence-numbered like the assign picker's search: typing is debounced but responses can still
   * arrive out of order, and a slow early page overwriting a fast later one would show the operator
   * rows they did not ask for.
   */
  const loadRoster = async () => {
    const seq = ++rosterSeq;
    const page = await api.listEmployeesPage(
      tenantId(),
      { limit: ROSTER_PAGE_SIZE, offset: rosterOffset() },
      rosterQuery().trim() || undefined,
      rosterSort(),
      rosterDescending() ? "desc" : "asc",
    );
    if (seq === rosterSeq) {
      setRoster(page);
    }
  };

  /** Reloads from the first page — what a write does, since it can change what page four holds. */
  const reloadRoster = async () => {
    setRosterOffset(0);
    await loadRoster();
  };

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      const [loadedRoles, loadedCatalogue] = await Promise.all([
        api.listRoles(tenantId()),
        api.permissionCatalogue(),
      ]);
      setRoles(loadedRoles);
      setCatalogue(loadedCatalogue);
      await loadRoster();
      await loadAssignments();
    } catch (caught) {
      await fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const loadAssignments = async () => {
    if (!storeId()) {
      setAssignments([]);
      return;
    }
    setAssignments(await api.listAssignmentsByStore(tenantId(), storeId()));
  };

  // Load on open and whenever the tenant changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());
  // Reload just the assignments when the store selection changes.
  onScopedContext("store", () => void loadAssignments().catch(fail));

  // An assignment carries the person it grants, so labelling one costs no roster read (ADR-0098,
  // B3-4). The fallback to the id is not defensive padding: the server returns a null name for an
  // assignment whose employee record is gone, and the grant is still real, so it has to be visible.
  const assignmentLabel = (row: Assignment) =>
    row.employee_name ? `${row.employee_name} (${row.employee_code ?? ""})`.trim() : row.employee_id;
  const roleLabel = (id: string) => roles().find((role) => role.role_template_id === id)?.name ?? id;

  const createEmployee = async () => {
    const code = newCode().trim();
    const name = newName().trim();
    if (!code || !name) {
      setError(t("people.codeNameRequired"));
      return;
    }
    setBusy(true);
    try {
      await api.createEmployee(tenantId(), code, name);
      setNewCode("");
      setNewName("");
      toast.ok(t("people.employeeCreated"));
      // Back to the first page: the roster is newest-first by default, so the person just added is
      // at the top of it and nowhere near wherever the operator was paged to.
      await reloadRoster();
    } catch (caught) {
      await fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const savePin = async () => {
    const employee = pinFor();
    if (!employee) {
      return;
    }
    const pin = pinValue().trim();
    if (!/^\d{4,8}$/.test(pin)) {
      setError(t("people.pinInvalid"));
      return;
    }
    setBusy(true);
    try {
      await api.setEmployeePin(employee.employee_id, tenantId(), pin);
      setPinFor(null);
      setPinValue("");
      toast.ok(t("people.pinSet"));
      // Same row, same page — re-read where the operator is rather than moving them.
      await loadRoster();
    } catch (caught) {
      await fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const setStatus = async (
    employee: Employee,
    status: "active" | "archived",
    doneMessage: string,
  ) => {
    setBusy(true);
    try {
      await api.updateEmployee(
        employee.employee_id,
        tenantId(),
        { name: employee.name, status },
        employee.etag,
      );
      setPendingArchive(null);
      toast.ok(doneMessage);
      // Archiving changes a status, not membership: the row stays on the page it was on.
      await loadRoster();
    } catch (caught) {
      await fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const openNewRole = () => {
    setRoleDraftId("");
    setRoleDraftEtag("");
    setRoleName("");
    setRolePermissions([]);
    setRoleOpen(true);
  };
  const openEditRole = (role: RoleTemplate) => {
    setRoleDraftId(role.role_template_id);
    setRoleDraftEtag(role.etag);
    setRoleName(role.name);
    setRolePermissions([...role.permissions]);
    setRoleOpen(true);
  };
  const togglePermission = (id: string, on: boolean) => {
    setRolePermissions((current) =>
      on ? [...current, id] : current.filter((existing) => existing !== id),
    );
  };

  const saveRole = async () => {
    const name = roleName().trim();
    if (!name) {
      setError(t("people.roleNameRequired"));
      return;
    }
    setBusy(true);
    try {
      if (roleDraftId()) {
        await api.updateRole(
          roleDraftId(),
          tenantId(),
          {
            name,
            permissions: rolePermissions(),
            status: "active",
          },
          roleDraftEtag(),
        );
        toast.ok(t("people.roleUpdated"));
      } else {
        await api.createRole(tenantId(), name, rolePermissions());
        toast.ok(t("people.roleCreated"));
      }
      setRoleOpen(false);
      await load();
    } catch (caught) {
      await fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const archiveRole = async (role: RoleTemplate) => {
    setBusy(true);
    try {
      await api.updateRole(
        role.role_template_id,
        tenantId(),
        {
          name: role.name,
          permissions: [...role.permissions],
          status: role.status === "archived" ? "active" : "archived",
        },
        role.etag,
      );
      toast.ok(t("people.roleUpdated"));
      await load();
    } catch (caught) {
      await fail(caught);
    } finally {
      setBusy(false);
    }
  };

  /// How many matches the picker asks for. Enough that a specific search lands the person on the
  /// first screen of results; small enough that a vague one does not pull a page of T1 rows the
  /// operator never looks at.
  const ASSIGN_MATCH_LIMIT = 12;

  // Debounced so a five-letter name is one request rather than five, and sequence-numbered so a slow
  // early response cannot overwrite a fast later one — the classic out-of-order search bug.
  const searchForAssignee = (text: string) => {
    setAssignSearch(text);
    clearTimeout(assignSearchTimer);
    const needle = text.trim();
    if (!needle) {
      setAssignMatches([]);
      return;
    }
    const seq = ++assignSearchSeq;
    assignSearchTimer = setTimeout(() => {
      void api
        .listEmployeesPage(tenantId(), { limit: ASSIGN_MATCH_LIMIT }, needle)
        .then((page) => {
          if (seq === assignSearchSeq) {
            setAssignMatches(page.items);
          }
        })
        .catch((caught: unknown) => void fail(caught));
    }, 250);
  };

  const chooseAssignee = (employee: Employee) => {
    setAssignChoice(employee);
    setAssignEmployee(employee.employee_id);
    setAssignSearch("");
    setAssignMatches([]);
  };

  const clearAssignee = () => {
    setAssignChoice(null);
    setAssignEmployee("");
    setAssignSearch("");
    setAssignMatches([]);
  };

  const createAssignment = async () => {
    if (!storeId()) {
      setError(t("people.assignNeedsStore"));
      return;
    }
    if (!assignEmployee() || !assignRole()) {
      setError(t("people.assignNeedsBoth"));
      return;
    }
    setBusy(true);
    try {
      await api.createAssignment(tenantId(), assignEmployee(), storeId(), assignRole());
      clearAssignee();
      setAssignRole("");
      toast.ok(t("people.assigned"));
      await loadAssignments();
    } catch (caught) {
      await fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const publish = async () => {
    if (!storeId()) {
      setError(t("people.assignNeedsStore"));
      return;
    }
    setBusy(true);
    try {
      await api.publishPermissions(tenantId(), storeId());
      toast.ok(t("people.published"));
    } catch (caught) {
      await fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const removeAssignment = async () => {
    const assignment = pendingRemove();
    if (!assignment) {
      return;
    }
    setBusy(true);
    try {
      await api.removeAssignment(tenantId(), assignment.assignment_id);
      setPendingRemove(null);
      toast.ok(t("people.unassigned"));
      await loadAssignments();
    } catch (caught) {
      await fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const groupedCatalogue = () => {
    const groups = new Map<string, PermissionInfo[]>();
    for (const info of catalogue()) {
      const list = groups.get(info.group) ?? [];
      list.push(info);
      groups.set(info.group, list);
    }
    return [...groups.entries()].map(([group, items]) => ({ group, items }));
  };

  // `sortField` is the server's token (`EmployeeSort`), `sortValue` the local comparator. Both are
  // given: the table is server-sorted, so only `sortField` is consulted, and `sortValue` is what the
  // headers would fall back to if this table ever went back to holding the whole roster.
  const employeeColumns = (): Column<Employee>[] => [
    {
      key: "name",
      header: t("people.name"),
      sortField: "name",
      sortValue: (row) => row.name,
      cell: (row) => <span>{row.name}</span>,
    },
    {
      key: "code",
      header: t("people.code"),
      sortField: "code",
      sortValue: (row) => row.code,
      cell: (row) => <span class="text-ink-muted">{row.code}</span>,
    },
    {
      key: "pin",
      header: t("people.pin"),
      cell: (row) => (
        <StatusBadge
          tone={row.has_pin ? "active" : "neutral"}
          label={row.has_pin ? t("people.pinSetBadge") : t("people.pinUnset")}
        />
      ),
    },
    {
      key: "status",
      header: t("people.status"),
      cell: (row) => (
        <StatusBadge
          tone={row.status === "archived" ? "archived" : "active"}
          label={row.status === "archived" ? t("status.archived") : t("status.active")}
        />
      ),
    },
    {
      key: "id",
      header: t("common.technicalDetails"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>
          {row.employee_id}
        </TechnicalDetails>
      ),
    },
  ];

  const roleColumns = (): Column<RoleTemplate>[] => [
    {
      key: "name",
      header: t("people.roleName"),
      sortValue: (row) => row.name,
      cell: (row) => <span>{row.name}</span>,
    },
    {
      key: "permissions",
      header: t("people.permissionCount"),
      sortValue: (row) => row.permissions.length,
      cell: (row) => <span class="text-ink-muted">{row.permissions.length}</span>,
    },
    {
      key: "status",
      header: t("people.status"),
      cell: (row) => (
        <StatusBadge
          tone={row.status === "archived" ? "archived" : "active"}
          label={row.status === "archived" ? t("status.archived") : t("status.active")}
        />
      ),
    },
    {
      key: "id",
      header: t("common.technicalDetails"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>
          {row.role_template_id}
        </TechnicalDetails>
      ),
    },
  ];

  return (
    <div>
      <PageHeader title={t("people.title")} description={t("people.description")} />
      <RequireContext need="tenant">
        <div class="flex flex-col gap-6">
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>

          <Card
            title={t("people.employees")}
            actions={
              <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
                {t("action.refresh")}
              </Button>
            }
          >
            <Show
              when={roster()}
              fallback={<p class="text-sm text-ink-muted">{t("people.loadHint")}</p>}
            >
              {(loaded) => (
                // Server-paged, server-searched and server-sorted (ADR-0098, #299). This screen used
                // to hold the tenant's whole roster for three readers at once; the other two are
                // gone — an assignment row names its own person, and the assign picker searches the
                // server — so the table is free to ask for twelve rows at a time.
                //
                // All four props travel together on purpose. `serverTotal`+`onPage` are what make
                // the pager count the set rather than the page; without `onSort` the headers would
                // go inert, and without `onQuery` the search box would filter the twelve rows on
                // screen while looking like it searched the roster. Each of those is a smaller lie
                // than an unpaged table, but a lie.
                <DataTable
                  columns={employeeColumns()}
                  rows={loaded().items}
                  searchText={(row) => `${row.name} ${row.code}`}
                  pageSize={ROSTER_PAGE_SIZE}
                  serverTotal={loaded().total}
                  onPage={(offset) => {
                    setRosterOffset(offset);
                    void loadRoster().catch(fail);
                  }}
                  onSort={(field, descending) => {
                    setRosterSort(field);
                    setRosterDescending(descending);
                    // A page-four offset means nothing once the order changes.
                    void reloadRoster().catch(fail);
                  }}
                  onQuery={(text) => {
                    setRosterQuery(text);
                    clearTimeout(rosterQueryTimer);
                    // Debounced like the assign picker: the kit fires this per keystroke, because it
                    // cannot know what a request costs the caller.
                    rosterQueryTimer = setTimeout(() => {
                      void reloadRoster().catch(fail);
                    }, 250);
                  }}
                  empty={<EmptyState title={t("people.employeesEmpty")} />}
                  actionsHeader={t("common.actions")}
                  actions={(row) => (
                    <Show when={canManage()}>
                      <div class="flex flex-wrap gap-2">
                        <Button
                          variant="secondary"
                          disabled={busy()}
                          onClick={() => {
                            setPinFor(row);
                            setPinValue("");
                          }}
                        >
                          {t("people.setPin")}
                        </Button>
                        <Show
                          when={row.status === "archived"}
                          fallback={
                            <Button
                              variant="danger"
                              disabled={busy()}
                              onClick={() => setPendingArchive(row)}
                            >
                              {t("people.archive")}
                            </Button>
                          }
                        >
                          <Button
                            variant="secondary"
                            disabled={busy()}
                            onClick={() =>
                              void setStatus(row, "active", t("people.restored"))
                            }
                          >
                            {t("people.restore")}
                          </Button>
                        </Show>
                      </div>
                    </Show>
                  )}
                />
              )}
            </Show>
          </Card>

          <Show when={canManage()}>
            <Card title={t("people.addEmployee")}>
              <div class="flex flex-col gap-4">
                <TextField
                  label={t("people.code")}
                  value={newCode()}
                  onInput={setNewCode}
                  placeholder={t("people.codePlaceholder")}
                />
                <TextField
                  label={t("people.name")}
                  value={newName()}
                  onInput={setNewName}
                  placeholder={t("people.namePlaceholder")}
                />
                <Button disabled={busy()} onClick={() => void createEmployee()}>
                  {t("action.create")}
                </Button>
              </div>
            </Card>
          </Show>

          <Card
            title={t("people.roles")}
            actions={
              <Show when={canManage()}>
                <Button disabled={busy()} onClick={openNewRole}>
                  {t("people.addRole")}
                </Button>
              </Show>
            }
          >
            <DataTable
              columns={roleColumns()}
              rows={roles()}
              searchText={(row) => row.name}
              pageSize={12}
              empty={<EmptyState title={t("people.rolesEmpty")} />}
              actionsHeader={t("common.actions")}
              actions={(row) => (
                <Show when={canManage()}>
                  <div class="flex flex-wrap gap-2">
                    <Button variant="secondary" disabled={busy()} onClick={() => openEditRole(row)}>
                      {t("people.edit")}
                    </Button>
                    <Button
                      variant="secondary"
                      disabled={busy()}
                      onClick={() => void archiveRole(row)}
                    >
                      {row.status === "archived" ? t("people.restore") : t("people.archive")}
                    </Button>
                  </div>
                </Show>
              )}
            />
          </Card>

          <Card
            title={t("people.assignments")}
            actions={
              <Show when={canManage() && storeId()}>
                <Button disabled={busy()} onClick={() => void publish()}>
                  {t("people.publish")}
                </Button>
              </Show>
            }
          >
            <Show
              when={storeId()}
              fallback={<p class="text-sm text-ink-muted">{t("people.assignNeedsStore")}</p>}
            >
              <div class="flex flex-col gap-4">
                <p class="text-sm text-ink-muted">{t("people.publishHint")}</p>
                <Show when={canManage()}>
                  <div class="flex flex-wrap items-end gap-3">
                    {/*
                      Searched on the server, not filtered out of a loaded roster (ADR-0098, B3-4).
                      A `<select>` here had to hold every active employee, which is what stopped the
                      table above from ever being paged: page the table and the picker would offer
                      only whoever landed on the page.

                      An archived match is listed and disabled rather than filtered away. Dropping it
                      would leave an operator searching for a real person and being shown nothing,
                      with no way to tell "no such person" from "that person is archived".
                    */}
                    <div class="block w-72">
                      <span class="mb-1 block text-sm font-medium text-ink">
                        {t("people.employee")}
                      </span>
                      <Show
                        when={assignChoice()}
                        fallback={
                          <div>
                            <input
                              type="text"
                              class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                              aria-label={t("people.findEmployee")}
                              placeholder={t("people.findEmployee")}
                              value={assignSearch()}
                              onInput={(event) => searchForAssignee(event.currentTarget.value)}
                            />
                            <Show when={assignSearch().trim()}>
                              <ul class="mt-1 max-h-48 overflow-y-auto rounded-token border border-line bg-surface-raised">
                                <For each={assignMatches()}>
                                  {(match) => (
                                    <li>
                                      <button
                                        type="button"
                                        disabled={match.status === "archived"}
                                        onClick={() => chooseAssignee(match)}
                                        class="flex min-h-touch w-full items-center justify-between gap-2 px-3 text-left text-sm text-ink hover:bg-surface disabled:text-ink-muted"
                                      >
                                        <span>
                                          {match.name} ({match.code})
                                        </span>
                                        <Show when={match.status === "archived"}>
                                          <StatusBadge
                                            tone="archived"
                                            label={t("status.archived")}
                                          />
                                        </Show>
                                      </button>
                                    </li>
                                  )}
                                </For>
                                <Show when={assignMatches().length === 0}>
                                  <li class="px-3 py-2 text-sm text-ink-muted">
                                    {t("people.noEmployeeMatch")}
                                  </li>
                                </Show>
                              </ul>
                            </Show>
                          </div>
                        }
                      >
                        {(chosen) => (
                          <div class="flex min-h-touch items-center justify-between gap-2 rounded-token border border-line bg-surface-raised px-3">
                            <span class="text-base text-ink">
                              {chosen().name} ({chosen().code})
                            </span>
                            <Button variant="secondary" onClick={clearAssignee}>
                              {t("people.changeEmployee")}
                            </Button>
                          </div>
                        )}
                      </Show>
                    </div>
                    <label class="block">
                      <span class="mb-1 block text-sm font-medium text-ink">
                        {t("people.role")}
                      </span>
                      <select
                        class="min-h-touch rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                        aria-label={t("people.role")}
                        value={assignRole()}
                        onChange={(event) => setAssignRole(event.currentTarget.value)}
                      >
                        <option value="">{t("people.choose")}</option>
                        <For each={roles().filter((r) => r.status === "active")}>
                          {(role) => <option value={role.role_template_id}>{role.name}</option>}
                        </For>
                      </select>
                    </label>
                    <Button disabled={busy()} onClick={() => void createAssignment()}>
                      {t("people.assign")}
                    </Button>
                  </div>
                </Show>
                <DataTable
                  columns={[
                    {
                      key: "employee",
                      header: t("people.employee"),
                      cell: (row: Assignment) => <span>{assignmentLabel(row)}</span>,
                    },
                    {
                      key: "role",
                      header: t("people.role"),
                      cell: (row: Assignment) => (
                        <span class="text-ink-muted">{roleLabel(row.role_template_id)}</span>
                      ),
                    },
                  ]}
                  rows={assignments()}
                  empty={<EmptyState title={t("people.assignmentsEmpty")} />}
                  actionsHeader={t("common.actions")}
                  actions={(row) => (
                    <Show when={canManage()}>
                      <Button
                        variant="danger"
                        disabled={busy()}
                        onClick={() => setPendingRemove(row)}
                      >
                        {t("people.unassign")}
                      </Button>
                    </Show>
                  )}
                />
              </div>
            </Show>
          </Card>
        </div>

        <Modal
          open={pinFor() !== null}
          title={t("people.setPinTitle")}
          closeLabel={t("action.close")}
          onClose={() => setPinFor(null)}
          footer={
            <>
              <Button variant="secondary" onClick={() => setPinFor(null)}>
                {t("action.cancel")}
              </Button>
              <Button disabled={busy()} onClick={() => void savePin()}>
                {t("people.setPin")}
              </Button>
            </>
          }
        >
          <div class="flex flex-col gap-3">
            <p class="text-sm text-ink-muted">{t("people.pinHint")}</p>
            <TextField
              label={t("people.pin")}
              type="password"
              value={pinValue()}
              onInput={setPinValue}
              placeholder={t("people.pinPlaceholder")}
            />
          </div>
        </Modal>

        <Drawer
          open={roleOpen()}
          title={roleDraftId() ? t("people.editRole") : t("people.addRole")}
          closeLabel={t("action.close")}
          onClose={() => setRoleOpen(false)}
          footer={
            <>
              <Button variant="secondary" onClick={() => setRoleOpen(false)}>
                {t("action.cancel")}
              </Button>
              <Button disabled={busy()} onClick={() => void saveRole()}>
                {t("action.save")}
              </Button>
            </>
          }
        >
          <div class="flex flex-col gap-4">
            <TextField
              label={t("people.roleName")}
              value={roleName()}
              onInput={setRoleName}
              placeholder={t("people.roleNamePlaceholder")}
            />
            <FormField label={t("people.permissions")}>
              <div class="flex flex-col gap-4">
                <For each={groupedCatalogue()}>
                  {(bucket) => (
                    <div>
                      <p class="mb-1 text-xs font-semibold uppercase tracking-wide text-ink-muted">
                        {bucket.group}
                      </p>
                      <div class="flex flex-col gap-1">
                        <For each={bucket.items}>
                          {(info) => (
                            <label class="flex items-start gap-2 text-sm text-ink">
                              <input
                                type="checkbox"
                                class="mt-1"
                                aria-label={info.id}
                                checked={rolePermissions().includes(info.id)}
                                onChange={(event) =>
                                  togglePermission(info.id, event.currentTarget.checked)
                                }
                              />
                              <span>
                                <span class="font-medium">{info.id}</span>
                                <span class="block text-xs text-ink-muted">{info.description}</span>
                              </span>
                            </label>
                          )}
                        </For>
                      </div>
                    </div>
                  )}
                </For>
              </div>
            </FormField>
          </div>
        </Drawer>

        <ConfirmDialog
          open={pendingArchive() !== null}
          title={t("people.archiveTitle")}
          message={t("people.archiveMessage")}
          confirmLabel={t("people.archive")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={busy()}
          onConfirm={() => {
            const employee = pendingArchive();
            if (employee) {
              void setStatus(employee, "archived", t("people.archived"));
            }
          }}
          onCancel={() => setPendingArchive(null)}
        />

        <ConfirmDialog
          open={pendingRemove() !== null}
          title={t("people.unassignTitle")}
          message={t("people.unassignMessage")}
          confirmLabel={t("people.unassign")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={busy()}
          onConfirm={() => void removeAssignment()}
          onCancel={() => setPendingRemove(null)}
        />
      </RequireContext>
    </div>
  );
}
