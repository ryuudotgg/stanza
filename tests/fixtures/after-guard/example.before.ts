function publish(draft: Draft) {
  if (!draft.title) throw new Error("title required");
  const slug = slugify(draft.title);
  if (exists(slug)) throw new Error(`${slug} is taken`);
  save(draft, slug);
  notify(draft.author);
}
