import { addTodo } from "./todos";

export function TodoList({ todos }: { todos: string[] }) {
  const count = todos.length;

  if (count === 0) return null;

  return (
    <section>
      <button
        type="button"
        onClick={() => {
          const todo = todos[0];

          if (!todo) return;

          addTodo(todo);
        }}
      >
        Add first todo
      </button>

      <ul>
        {todos.map((todo) => {
          const label = todo.trim();

          if (!label) {
            return null;
          }

          return <li key={todo}>{label}</li>;
        })}
      </ul>
    </section>
  );
}
