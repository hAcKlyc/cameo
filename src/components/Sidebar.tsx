import { useEffect, useRef, useState } from "react";
import { Plus, FolderPlus, FolderOpen, MoreHorizontal, Pencil, Trash2 } from "lucide-react";
import { useWorkspaceStore } from "../store/workspace";
import { useBoardStore } from "../store/board";
import { ipc } from "../lib/ipc";
import { useT } from "../i18n/locale";
import type { WorkspaceEntry } from "../types";

function WorkspaceRow({ w, active }: { w: WorkspaceEntry; active: boolean }) {
  const [editing, setEditing] = useState(false);
  const [menu, setMenu] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const t = useT();

  useEffect(() => {
    if (editing) {
      inputRef.current?.focus();
      inputRef.current?.select();
    }
  }, [editing]);

  const commit = (v: string) => {
    setEditing(false);
    if (v.trim() && v.trim() !== w.name) void useWorkspaceStore.getState().rename(w.id, v.trim());
  };

  return (
    <div className={`cm-ws${active ? " is-active" : ""}`}>
      <button
        className="cm-ws__main"
        title={w.path}
        onClick={() => !editing && void useWorkspaceStore.getState().openWorkspace(w.path)}
        onDoubleClick={() => setEditing(true)}
      >
        <span className="cm-ws__dot" data-kind={w.kind} aria-hidden />
        {editing ? (
          <input
            ref={inputRef}
            className="cm-ws__edit"
            defaultValue={w.name}
            onClick={(e) => e.stopPropagation()}
            onBlur={(e) => commit(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") commit((e.target as HTMLInputElement).value);
              if (e.key === "Escape") setEditing(false);
            }}
          />
        ) : (
          <span className="cm-ws__name">{w.name}</span>
        )}
      </button>
      <button
        className="cm-ws__more"
        title={t("sidebar.more")}
        onClick={(e) => {
          e.stopPropagation();
          setMenu((m) => !m);
        }}
      >
        <MoreHorizontal size={15} />
      </button>
      {menu && (
        <div className="cm-ws__menu" onMouseLeave={() => setMenu(false)}>
          <button
            onClick={() => {
              setMenu(false);
              setEditing(true);
            }}
          >
            <Pencil size={13} />
            {t("sidebar.rename")}
          </button>
          <button
            onClick={() => {
              setMenu(false);
              void ipc.openDir(w.path);
            }}
          >
            <FolderOpen size={13} />
            {t("sidebar.openDir")}
          </button>
          <button
            className="cm-ws__del"
            onClick={() => {
              setMenu(false);
              void useWorkspaceStore.getState().remove(w.id);
            }}
          >
            <Trash2 size={13} />
            {t("sidebar.remove")}
          </button>
        </div>
      )}
    </div>
  );
}

export function Sidebar() {
  const sidebarOpen = useWorkspaceStore((s) => s.sidebarOpen);
  const workspaces = useWorkspaceStore((s) => s.workspaces);
  const activeId = useBoardStore((s) => s.boardId);
  const t = useT();
  if (!sidebarOpen) return null;
  return (
    <div className="cm-sidebar">
      <div className="cm-sidebar__actions">
        <button className="cm-sidebar__action" onClick={() => void useWorkspaceStore.getState().newWorkspace()}>
          <Plus size={15} />
          {t("sidebar.new")}
        </button>
        <button className="cm-sidebar__action" onClick={() => void useWorkspaceStore.getState().addFolder()}>
          <FolderPlus size={15} />
          {t("sidebar.addFolder")}
        </button>
      </div>
      <div className="cm-sidebar__head">{t("sidebar.workspaces")}</div>
      <div className="cm-sidebar__list">
        {workspaces.length === 0 && <div className="cm-sidebar__empty">{t("sidebar.empty")}</div>}
        {workspaces.map((w) => (
          <WorkspaceRow key={w.id} w={w} active={w.id === activeId} />
        ))}
      </div>
    </div>
  );
}
