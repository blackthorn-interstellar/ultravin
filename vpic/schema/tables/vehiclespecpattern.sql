CREATE TABLE vpic.vehiclespecpattern (
    id integer NOT NULL,
    vspecschemapatternid integer NOT NULL,
    iskey boolean DEFAULT false NOT NULL,
    elementid integer NOT NULL,
    attributeid character varying(500) NOT NULL,
    createdon timestamp without time zone,
    updatedon timestamp without time zone
);
