function submit(form: Form) {
  const payload = serialize(form);
  for (const field of form.fields) {
    field.disabled = true;
    field.touched = false;
  }

  send(payload);
}
