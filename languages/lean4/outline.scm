(declaration
  (def "def" @context name: (identifier) @name)) @item

(declaration
  (theorem "theorem" @context name: (identifier) @name)) @item
(declaration
  (theorem "lemma" @context name: (identifier) @name)) @item

(declaration
  (abbrev "abbrev" @context name: (identifier) @name)) @item

(declaration
  (opaque "opaque" @context name: (identifier) @name)) @item

(declaration
  (constant "constant" @context name: (identifier) @name)) @item

(declaration
  (axiom "axiom" @context name: (identifier) @name)) @item

(declaration
  (instance "instance" @context name: (identifier) @name)) @item

(declaration
  (structure "structure" @context name: (identifier) @name)) @item
(declaration
  (structure "class" @context name: (identifier) @name)) @item

(declaration
  (inductive "inductive" @context name: (identifier) @name)) @item

(field name: (identifier) @name) @item
(ctor_alt "|" @context name: (identifier) @name) @item

(namespace "namespace" @context name: (identifier) @name) @item
(section "section" @context name: (identifier) @name) @item
