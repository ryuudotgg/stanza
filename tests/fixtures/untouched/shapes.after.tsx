interface Props {
  a: {
    b: number;

    c: string;
  };

  d: number;
}

type T = {
  nested: {
    deep: boolean;
  };
  other: string;
};

const obj = {
  a: 1,

  b: {
    c: 2,
  },
};
class C {
  a = 1;

  b = {
    c: 2,
  };
  method() {
    return <div a={obj.a} style={{
      color: "red",
    }}>{obj.b.c}</div>;
  }
}
export function View(props: Props) {
  return (
    <section
      data-a={props.a.b}
      onClick={() => {
        track(props.d);
      }}
    >
      {props.d}
    </section>
  );
}
enum E {
  A,

  B,
}
declare module "x" {
  export const y: number;

  export const z: number;
}
const top = 1;
const next = compute(
  top,
);
use(next);
