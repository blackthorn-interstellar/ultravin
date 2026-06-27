CREATE TABLE vpic.defaultvalue (
    id integer NOT NULL,
    elementid integer NOT NULL,
    vehicletypeid integer NOT NULL,
    defaultvalue character varying(500),
    createdon timestamp without time zone,
    updatedon timestamp without time zone
);
