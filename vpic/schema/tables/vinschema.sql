CREATE TABLE vpic.vinschema (
    id integer NOT NULL,
    name character varying(255) NOT NULL,
    sourcewmi character varying(6),
    createdon timestamp without time zone,
    updatedon timestamp without time zone,
    tobeqced boolean
);
