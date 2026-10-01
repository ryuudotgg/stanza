export function Named(name: string) {
  track(name);
  warm(name);
  const Icon = icons[name];
  return <Icon />;
}

export function Member(name: string) {
  track(name);
  warm(name);
  const ui = kit(name);
  return <ui.Button />;
}

export function Closing(name: string) {
  track(name);
  warm(name);
  const Frame = frames[name];
  return <Frame>{(Frame) => <Frame />}</Frame>;
}

export class Panel {
  render(name: string) {
    track(name);
    warm(name);
    this.ui = kit(name);
    return <this.ui.Button />;
  }
}

export function Intrinsic(name: string) {
  track(name);
  warm(name);
  const div = make(name);

  return <div />;
}

export function Attribute(name: string) {
  track(name);
  warm(name);
  const title = make(name);

  return <Named title />;
}

export function Nested(name: string) {
  track(name);
  warm(name);
  const Icon = icons[name];

  return <div>{() => <Icon />}</div>;
}

export function Shadowed(name: string) {
  track(name);
  warm(name);
  const Icon = icons[name];

  if (name) {
    const Icon = other(name);
    mount(<Icon />);
  }
}

export function Underscored(name: string) {
  track(name);
  warm(name);
  const _Icon = icons[name];
  return <_Icon />;
}

export function Hyphenated(name: string) {
  track(name);
  warm(name);
  const Custom = make(name);

  return <Custom-el />;
}
