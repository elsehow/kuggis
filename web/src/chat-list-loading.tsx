export function ChatListLoading() {
  return <div className="chat-list-loading" role="status" aria-label="Loading chats">
    <span className="visually-hidden">Loading chats…</span>
    {[0, 1, 2, 3, 4].map(row => <div key={row} className="chat-skeleton" aria-hidden="true" />)}
  </div>;
}
