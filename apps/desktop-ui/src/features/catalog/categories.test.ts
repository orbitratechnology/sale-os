import { describe, expect, it } from "vitest";
import type { CategoryDto } from "../../ipc/bindings";
import { flatTree } from "./categories";

const cat = (categoryId: string, name: string, parentId: string | null, archived = false): CategoryDto => ({ categoryId, name, parentId, archived, version: 1 });

describe("flatTree", () => {
  it("lists parents before children, siblings by name, with depth; archived ones are left out", () => {
    const all = [
      cat("s", "Soft drinks", "d"),
      cat("d", "Drinks", null),
      cat("a", "Alcohol", "d"),
      cat("f", "Fizzy", "s"),
      cat("b", "Bakery", null),
      cat("x", "Old", null, true),
      cat("y", "Under old", "x"),
    ];
    expect(flatTree(all).map((t) => `${t.depth}:${t.category.name}`)).toEqual(["1:Bakery", "1:Drinks", "2:Alcohol", "2:Soft drinks", "3:Fizzy"]);
  });
});
